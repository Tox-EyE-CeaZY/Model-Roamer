# Model Roamer

An AI orchestration studio in the Roamer suite, alongside [Board Roamer](https://github.com/Tox-EyE-CeaZY/Board-Roamer)
and [Web Roamer](https://github.com/Tox-EyE-CeaZY/Web-Roamer), with the same look: chamfered panels, the contour-map
background and the **MODEL/ROAMER** title bar.

One **lead** agent plans the work and runs **worker threads** in Claude Code, Codex, Gemini CLI and Antigravity, all at
once, each on its own git branch so they never step on each other. You watch, answer the questions they raise, and
merge what you like.

## Get it

Grab the file for your computer from the [latest release](https://github.com/Tox-EyE-CeaZY/Model-Roamer/releases/latest):

| System | File |
| --- | --- |
| Windows 10/11 (most laptops) | `model-roamer-windows-x64.exe` |
| Windows on ARM | `model-roamer-windows-arm64.exe` |
| macOS (Apple silicon / Intel) | `ModelRoamer-macos-arm64.zip` / `ModelRoamer-macos-x64.zip` |
| Linux desktop | `model-roamer-linux-x64` (needs `libwebkit2gtk-4.1`) |
| Linux server, no window (x64 / ARM, e.g. a Raspberry Pi) | `model-roamer-server-linux-x64` / `model-roamer-server-linux-arm64` |

Each one is a single file with nothing to install. Windows uses the Edge WebView2 runtime that comes with Windows 10 and
11. The exe isn't code-signed, so SmartScreen may warn the first time: **More info → Run anyway**.

The first launch walks you through it: which agents to set up, signing in to each, and connecting GitHub. You never
need a terminal. Agents › **Install** fetches a CLI for you, and **Sign in** runs its login in a window inside the app.

## What it does

- **Projects** are a folder or a GitHub repository, plus instructions every agent reads. Each has a **lead** (Claude by
  default; Codex and Gemini can lead too) whose chat sits on the project's board.
- **Worker threads**: the lead starts them with the agent and model that suit the job. Each gets its own git worktree
  and branch, and claims the files it is changing, so two workers never edit the same thing. The board shows them in
  lanes: *Needs you*, *Working*, *Paused*, *Done*.
- **Per-harness guides**: every agent gets detailed instructions for driving the others (prompts/), so any lead can
  orchestrate any mix.
- **Decisions**: when an agent needs you, it asks with a card of options. Your answer wakes it up again.
- **Threads like email**: open any thread in its own window (Ctrl+Shift+O), or make pop-out the default under
  Settings › Windows. Several main windows work too.
- **Review and merge**: see a thread's changes as a diff, merge its branch, or open a pull request. The Git tab lists the
  project's pull requests and merges them on GitHub. Model Roamer signs in to GitHub itself.
- **Routing rules**: send kinds of work (frontend, backend, tests, docs, research…) to the agent you prefer, globally or per project.
- **Limit fallback**: when an agent hits its plan's usage limit, its threads move to the agent routing picks next. With
  fallback off, they wait and carry on when the limit resets.
- **Artifacts**: pages, images and files that agents publish, in a gallery and a viewer, routed to the project they
  belong to.
- **Memory**: global and per-project notes that agents read and write. You can search, edit and restore them from the
  trash.
- **Usage**: turns and tokens per agent per day, failures, and when each one hit its limit.
- **Chats**: plain conversations with any agent, outside a project.
- Undo for deletes, a command palette (Ctrl+K), keyboard shortcuts for everything (press **?**), themes and an accent
  colour, and it **updates itself** from GitHub releases (Windows and Linux; macOS opens the release page).

Everything is kept in one SQLite database in your data folder: `%LOCALAPPDATA%\ModelRoamer` on Windows,
`~/Library/Application Support/ModelRoamer` on macOS, `~/.local/share/ModelRoamer` on Linux.

## Use it from other devices

Model Roamer can run on one machine (a desktop, a home server, a cloud box) and be used from any other: a laptop,
a phone's browser, or another copy of the desktop app.

**From the desktop app:** Settings › Remote access › **Share this computer**. It then lists the addresses it answers
on and shows a pairing code and QR code.

**As a headless server:**

```
model-roamer server                      # listens on 0.0.0.0:4630 and prints a pairing code
model-roamer server --port 8080 --name studio
model-roamer pair                        # a fresh code from the running server
```

On the other device, open `http://<server>:4630` in a browser (or **Connect** in the desktop app's server menu) and
enter the code. Desktop apps on the same network find servers on their own.

How to reach it:

- **Same network (LAN)**: use the address it shows, like `http://192.168.1.20:4630`, or the machine's name
  (`http://studio.local:4630`).
- **Tailscale**: install Tailscale on both devices and use the server's Tailscale name, like
  `http://studio.tailnet-name.ts.net:4630`. Nothing to open on your router. For HTTPS, run `tailscale cert` and pass the
  files with `--tls-cert` and `--tls-key`.
- **WireGuard**: connect the tunnel, then use the server's tunnel address, like `http://10.0.0.1:4630`.
- **Your own domain**: put it behind a reverse proxy that handles HTTPS (Caddy: `studio.example.com { reverse_proxy
  127.0.0.1:4630 }`), or serve HTTPS directly with `--tls-cert fullchain.pem --tls-key privkey.pem`. WebSockets must
  pass through (`/api/ws`).

Every device has to pair, codes expire and wrong guesses are rate-limited, and paired devices can be removed under
Settings › Remote access. If the server can be reached from the internet, use HTTPS.

Running it as a service on Linux (systemd):

```
[Unit]
Description=Model Roamer
After=network-online.target

[Service]
ExecStart=/usr/local/bin/model-roamer-server-linux-x64 server
User=you
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

## Build it

Needs Rust (stable). On Linux also: `libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev libxdo-dev
libayatana-appindicator3-dev libssl-dev`.

```
cargo run                                    # the desktop app
cargo run -- --browser                       # the app in your browser, at http://127.0.0.1:4625
cargo run -- server                          # the headless server
cargo build --release --no-default-features  # a server-only build, no window libraries needed
cargo test --no-default-features
```

The interface is plain HTML, CSS and JavaScript modules in `ui/`, with no build step. Debug builds read it straight from
disk, so a reload shows your changes. `tools/make_icon.py` draws the icon.

Every push to `main` builds Windows, macOS and Linux and publishes them as the release for the version in
`Cargo.toml`. A `v*` tag publishes that version.

## License

MIT
