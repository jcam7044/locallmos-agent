# Running the LocalLMOS agent as a service

The agent is one binary with three modes:

| Command | Mode |
| --- | --- |
| `locallmos-agent` | GUI tray app (enrollment + status) |
| `locallmos-agent service` | headless worker loops (for systemd/launchd/Windows) |
| `locallmos-agent enroll --code <CODE> --name <NAME>` | headless enrollment, then exit |

**When to use which:**
- **Dedicated / headless rigs** → run the **service** (starts at boot, no login needed). Enroll once via the CLI.
- **Workstations you log into** → the **tray app** is enough; it autostarts to the tray on login (launched with `--minimized`) and hides to the tray when you close the window.

The service runs as **root** by default so it can restart the runtime (`systemctl restart ollama`) and reboot the machine. Drop to an unprivileged `User=` if you don't need those control actions.

Credentials (`config.json`) and Supabase settings share a config dir via
`LOCALLMOS_CONFIG_DIR` so the `service` and `enroll` invocations agree.

### Config location — GUI vs service

The two run modes resolve **different** config dirs unless you tell them
otherwise, so credentials enrolled in one are not visible to the other:

- **Headless service / CLI enroll** use `LOCALLMOS_CONFIG_DIR` (the installers
  set it, e.g. `/etc/locallmos-agent` on Linux/macOS, `C:\ProgramData\locallmos-agent`
  on Windows).
- **Tray GUI** has no such env by default, so it falls back to the per-user OS
  config dir (`~/.config/locallmos-agent`, `~/Library/Application Support/locallmos-agent`,
  or `%APPDATA%\locallmos-agent`).

This is intentional: the tray app is a per-user, workstation convenience while the
service is a system-wide daemon. Pick one mode per machine. If you want the GUI and
the service to share one enrollment, launch the GUI with the same
`LOCALLMOS_CONFIG_DIR` the service uses. The agent logs its resolved config dir at
startup (`agent config dir: …`) so you can confirm which store is active.

---

## Linux (systemd)

The public installer defaults to the desktop tray app. Pass `--service` for a
headless system service:

```bash
curl -fsSL https://locallmos.com/install.sh | sh -s -- --service --code <PAIRING_CODE> --name "Basement 3090"
```

Scripted:

```bash
# Generate a pairing code in the dashboard first.
cd apps/agent/service
./install-service.sh --code <PAIRING_CODE> --name "Basement 3090"
```

This builds a release binary, installs it to `/usr/local/bin`, writes
`/etc/locallmos-agent/agent.env` (fill in your Supabase URL + anon key),
installs the unit, enrolls, and enables the service.

### Choosing the runtime

By default the agent drives **Ollama**. To run **llama.cpp** instead — which gives
native, grammar-constrained tool calling (the same mechanism Codex/OpenCode rely
on) — pass `--runtime llamacpp`:

```bash
curl -fsSL https://locallmos.com/install.sh | sh -s -- \
  --service --runtime llamacpp --code <PAIRING_CODE> --name "Basement 3090"
```

The installer auto-detects your hardware, downloads a prebuilt `llama-server`
(accelerated Vulkan build where a GPU is present, else CPU) into
`/opt/locallmos/llama`, creates a models directory at `/var/lib/locallmos/models`,
and writes the `LOCALLMOS_RUNTIME=llamacpp` + `LOCALLMOS_LLAMACPP_*` vars into
`agent.env`. Drop a `.gguf` into the models directory, then select it in the
dashboard. Pin a specific engine build with `--llamacpp-version bNNNNN` (default:
a known-good pinned release; `latest` resolves the newest).

Model launch settings are configured per GGUF in the tray app's **Models** tab.
Leaving them on **Recommended** lets llama.cpp auto-fit context and GPU offload
to the available hardware. The installer no longer forces a global GPU-layer
count; advanced unrelated flags can still be supplied with
`LOCALLMOS_LLAMACPP_ARGS`.

GGUFs with embedded Multi-Token Prediction layers are detected from their model
metadata. The per-model **Recommended** speculative-decoding setting enables
llama.cpp `draft-mtp` for those models and leaves it off for other GGUFs.

Manual equivalent:

```bash
cargo build --release --manifest-path apps/agent/src-tauri/Cargo.toml
sudo install -m755 apps/agent/src-tauri/target/release/locallmos-agent /usr/local/bin/
sudo mkdir -p /etc/locallmos-agent
sudo cp apps/agent/service/agent.env.example /etc/locallmos-agent/agent.env   # then edit
sudo cp apps/agent/service/locallmos-agent.service /etc/systemd/system/
sudo systemctl daemon-reload
# enroll (config lands in /etc/locallmos-agent)
sudo env LOCALLMOS_CONFIG_DIR=/etc/locallmos-agent bash -c \
  'set -a; source /etc/locallmos-agent/agent.env; set +a; \
   /usr/local/bin/locallmos-agent enroll --code <CODE> --name "My Rig"'
sudo systemctl enable --now locallmos-agent
```

Observe / manage:

```bash
systemctl status locallmos-agent
journalctl -u locallmos-agent -f
sudo systemctl restart locallmos-agent
```

Uninstall: `./uninstall-service.sh` (add `--purge` to also remove credentials).

> If you'd rather run as your own user without root, use a **user service** with
> lingering so it runs while logged out: put the unit in `~/.config/systemd/user/`,
> `loginctl enable-linger $USER`, then `systemctl --user enable --now locallmos-agent`.
> Note: an unprivileged user usually can't restart a system-level Ollama or reboot.

---

## macOS (launchd)

The public installer defaults to the desktop tray app. Pass `--service` for a
headless launchd daemon:

```bash
curl -fsSL https://locallmos.com/install.sh | sh -s -- --service --code <PAIRING_CODE> --name "Mac Studio"
```

```bash
cargo build --release --manifest-path apps/agent/src-tauri/Cargo.toml
sudo install -m755 apps/agent/src-tauri/target/release/locallmos-agent /usr/local/bin/
sudo mkdir -p /etc/locallmos-agent

# enroll once
sudo env LOCALLMOS_CONFIG_DIR=/etc/locallmos-agent \
  LOCALLMOS_SUPABASE_URL=https://<ref>.supabase.co \
  LOCALLMOS_SUPABASE_ANON_KEY=<anon> \
  /usr/local/bin/locallmos-agent enroll --code <CODE> --name "Mac Studio"

# install + load the daemon (edit the plist's Supabase vars first)
sudo cp apps/agent/service/os.locallmos.agent.plist /Library/LaunchDaemons/
sudo launchctl load -w /Library/LaunchDaemons/os.locallmos.agent.plist
```

Logs: `/var/log/locallmos-agent.log`. Unload: `sudo launchctl unload -w /Library/LaunchDaemons/os.locallmos.agent.plist`.

---

## Windows

The public installer defaults to the desktop tray app. Pass `-Service` for a
headless SYSTEM startup task:

```powershell
& ([scriptblock]::Create(((curl.exe -fsSL https://locallmos.com/install.ps1) -join "`n"))) -Service -Code <CODE> -Name "Windows Rig"
```

Build `apps\agent\src-tauri\target\release\locallmos-agent.exe`, then either:

**Task Scheduler (runs at startup as SYSTEM):**

```powershell
setx /M LOCALLMOS_CONFIG_DIR "C:\ProgramData\locallmos-agent"
setx /M LOCALLMOS_SUPABASE_URL "https://<ref>.supabase.co"
setx /M LOCALLMOS_SUPABASE_ANON_KEY "<anon>"

# enroll once (new shell so the env vars are present)
locallmos-agent.exe enroll --code <CODE> --name "Windows Rig"

schtasks /Create /TN "LocalLMOS Agent" /TR "C:\path\to\locallmos-agent.exe service" ^
  /SC ONSTART /RU SYSTEM /RL HIGHEST /F
schtasks /Run /TN "LocalLMOS Agent"
```

**Or** register a real Windows Service with a wrapper such as
[NSSM](https://nssm.cc/) pointing at `locallmos-agent.exe service`.
