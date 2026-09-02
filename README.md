# Portside

A macOS menu-bar app that shows the local dev servers your coding agents left
behind — and stops them safely.

Agents start a dev server, finish their task, and exit. The server survives,
unattended, sometimes for days. Some are still working; some have been holding a
port while serving nothing since Tuesday. Both are invisible from the outside.
Portside puts them all in one panel: which project each belongs to, whether it
still answers, how long it has been up, and which are safe to stop.

The name is nautical — ports, at your side.

## What it does

- **Sees every listening TCP port** on your machine, IPv4 and IPv6, with no
  password prompt. A port held by another user is shown as occupied, never as
  free.
- **Attributes each server to a project**, by walking up from its working
  directory to the nearest repository root. Where that can't be trusted, the
  guess is shown as a guess.
- **Checks whether each one still answers**, continuously, with a bare TCP
  connection that carries no protocol data — safe against databases and mail
  services. This is the signal that finds a genuinely dead server, and nothing
  else does.
- **Stops the forgotten ones, carefully.** Every stop is confirmed and names
  what it costs in your own words. A polite stop is verified against the port
  actually being released; force stop is a separate decision you make yourself,
  never an automatic escalation.
- **Leaves alone what isn't its business.** Servers belonging to macOS or to no
  project at all appear in a watch-only bay with no stop control anywhere in the
  layout.

Portside never claims a server is forgotten. That is your judgement; the app
shows the evidence.

## Built with

Tauri v2 — a Rust core with a small web panel. The frontend is vanilla ES
modules and plain CSS: no framework, no bundler, no runtime dependencies. Icons
are [Ant Design](https://github.com/ant-design/ant-design-icons) paths inlined
as SVG.

The Rust/UI boundary is a frozen contract in [docs/IPC.md](docs/IPC.md).
[CONTEXT.md](CONTEXT.md) defines every term the interface uses, and
[REQUIREMENTS.md](REQUIREMENTS.md) says what the app must do and why.

## Developing

Requires Node and a Rust toolchain.

```sh
npm install          # or pnpm install
npm run tauri dev    # or pnpm tauri dev
```

The frontend also runs standalone against an in-memory mock backend — useful for
UI work without building the Rust side. Serve `src/` over HTTP and open it:

```sh
python3 -m http.server 4173 --directory src
```

When `window.__TAURI__` is absent the app talks to `src/mock.js` instead, which
is shaped exactly like the IPC contract.

## Licence

Portside is free to use and its source is open to read, under the
[PolyForm Noncommercial 1.0.0](LICENSE) licence: use it, study it, fork it,
improve it — for any noncommercial purpose. Selling Portside or building a
commercial product on it is not permitted without a separate licence; if you
want one, write to the address below.

Contributions and issues are welcome.

If Portside saves you from one more `lsof -i :3000`, you can buy the author a
coffee — sponsor link coming with the first release. Entirely optional; the app
is complete without it.

## Contact

Purin Buriwong — askpurin@pm.me

macOS is the only target today. Windows and Linux are reachable by design (all
platform knowledge sits behind one interface) but unbuilt and unverified.
