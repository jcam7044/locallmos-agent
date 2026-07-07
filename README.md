# LocalLMOS Agent

The open-source agent for [LocalLMOS](https://locallmos.com) — a cross-platform
(Linux, macOS, Windows) [Tauri](https://tauri.app) app that monitors and controls
the local LLM runtimes on a machine ("rig").

Run it **standalone** as a local control panel for your own models, or **connect it
to the cloud** to manage a fleet of rigs, share models, build teams, orchestrate
agents, and expose OpenAI-compatible endpoints from the LocalLMOS dashboard.

## Install

**Linux / macOS (Apple Silicon):**
```sh
curl -fsSL https://locallmos.com/install.sh | sh
```

**Windows (elevated PowerShell):**
```powershell
irm https://locallmos.com/install.ps1 | iex
```

This installs a signed binary to `/usr/local/bin` (or `%ProgramFiles%\LocalLMOS`),
sets up a service, and — when you pass a pairing code — enrolls the rig. Binaries
are verified by SHA-256 and [minisign](https://jedisct1.github.io/minisign/)
signature; the agent re-verifies every self-update against an embedded public key.

To install and enroll in one step (pairing code from the dashboard):
```sh
curl -fsSL https://locallmos.com/install.sh | sh -s -- --code <CODE> --name "My Rig"
```

## Modes

| Command | Mode |
| --- | --- |
| `locallmos-agent` | GUI tray app (local dashboard + optional cloud enrollment) |
| `locallmos-agent service` | headless worker (systemd / launchd / Task Scheduler) |
| `locallmos-agent enroll --code <CODE> --name <NAME>` | headless enrollment |

## Build from source

Requires Rust and [pnpm](https://pnpm.io). On Linux you also need the WebKitGTK
stack (`libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev`).

```sh
pnpm install
pnpm build                              # build the tray UI (embedded by Tauri)
cargo build --release --manifest-path src-tauri/Cargo.toml
```

The version is derived from the git tag at release time (`scripts/set-version.mjs`);
`tauri.conf.json` inherits its version from `Cargo.toml`.

## License

[Apache-2.0](LICENSE).
