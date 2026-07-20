#!/bin/sh
# LocalLMOS agent installer (Linux + macOS). POSIX sh — runs under dash when
# invoked as `curl … | sh` (the shebang is ignored in that case anyway).
#
#   curl -fsSL https://get.locallmos.os/install.sh | sh -s -- \
#     --supabase-url https://<ref>.supabase.co --anon-key <ANON> \
#     --code <PAIRING_CODE> --name "My Rig"
#
# Downloads a prebuilt, signed agent binary from GitHub Releases, verifies it,
# and installs it to /usr/local/bin. By default this is a desktop install: enroll
# in the current user's config dir when --code is given, then launch the tray GUI.
# Pass --service for a headless systemd/launchd install instead.
# POSIX sh only: no `pipefail` (dash lacks it); `-e` still aborts on the `curl -f`
# download failures and on a checksum mismatch.
set -eu

# ---- defaults (override via flags or env) ---------------------------------
REPO="${LOCALLMOS_REPO:-jcam7044/locallmos-agent}" # GitHub owner/repo hosting releases
CHANNEL="${LOCALLMOS_CHANNEL:-stable}"
VERSION="latest"                                  # or an explicit vX.Y.Z tag
NAME="$(hostname 2>/dev/null || echo my-rig)"
CODE=""
MODE="${LOCALLMOS_INSTALL_MODE:-desktop}"          # desktop or service
NO_LAUNCH="${LOCALLMOS_NO_LAUNCH:-0}"
# Which local LLM engine the rig runs. "ollama" (default) leaves current installs
# unchanged; "llamacpp" provisions llama-server (native, grammar-constrained tool
# calling) and points the agent at it.
RUNTIME="${LOCALLMOS_RUNTIME:-ollama}"
LLAMACPP_REPO="${LOCALLMOS_LLAMACPP_REPO:-ggml-org/llama.cpp}"
LLAMACPP_VERSION="${LOCALLMOS_LLAMACPP_VERSION:-b10068}" # pinned release tag, or "latest"
# Production locallmos.com backend baked in as defaults (both are public values —
# the anon key ships in the web bundle and is gated by RLS). Override with
# --supabase-url / --anon-key or the LOCALLMOS_SUPABASE_* env vars.
SUPABASE_URL="${LOCALLMOS_SUPABASE_URL:-https://fvpjkpfshbvszbcknkqq.supabase.co}"
ANON_KEY="${LOCALLMOS_SUPABASE_ANON_KEY:-eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJzdXBhYmFzZSIsInJlZiI6ImZ2cGprcGZzaGJ2c3piY2tua3FxIiwicm9sZSI6ImFub24iLCJpYXQiOjE3ODI5NzI3MjYsImV4cCI6MjA5ODU0ODcyNn0.b0FDzCAweH6VIwcumLKjNP959unJCUN_egZpb7KdCwg}"

# minisign public key matching release CI's signing key. Keep in sync with
# RELEASE_PUBLIC_KEY in apps/agent/src-tauri/src/updater.rs.
PUBKEY="RWR+94+uka+PJB5Wbmak5GN2J+eZjIgoj3PGFH4dAoqhBuCfIFjBy6u7"

CONFIG_DIR="/etc/locallmos-agent"
BIN_DST="/usr/local/bin/locallmos-agent"

while [ $# -gt 0 ]; do
  case "$1" in
    --code) CODE="$2"; shift 2 ;;
    --name) NAME="$2"; shift 2 ;;
    --channel) CHANNEL="$2"; shift 2 ;;
    --version) VERSION="$2"; shift 2 ;;
    --repo) REPO="$2"; shift 2 ;;
    --supabase-url) SUPABASE_URL="$2"; shift 2 ;;
    --anon-key) ANON_KEY="$2"; shift 2 ;;
    --desktop) MODE="desktop"; shift ;;
    --service|--headless) MODE="service"; shift ;;
    --runtime) RUNTIME="$2"; shift 2 ;;
    --llamacpp-version) LLAMACPP_VERSION="$2"; shift 2 ;;
    --no-launch) NO_LAUNCH="1"; shift ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done
case "$MODE" in
  desktop|service) ;;
  *) echo "unknown install mode: $MODE (expected desktop or service)" >&2; exit 2 ;;
esac
case "$RUNTIME" in
  ollama|llamacpp) ;;
  *) echo "unknown runtime: $RUNTIME (expected ollama or llamacpp)" >&2; exit 2 ;;
esac
if [ "$MODE" = "desktop" ] && [ "$(id -u)" = "0" ]; then
  echo "desktop install must be run as your login user so the tray app can appear." >&2
  echo "Run without sudo: curl -fsSL https://locallmos.com/install.sh | sh" >&2
  echo "For a headless root service, pass --service." >&2
  exit 1
fi

# ---- platform detection ("{os}-{arch}", matching CI asset names) ----------
os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Linux)  OS="linux" ;;
  Darwin) OS="macos" ;;
  *) echo "unsupported OS: $os" >&2; exit 1 ;;
esac
case "$arch" in
  x86_64|amd64) ARCH="x86_64" ;;
  arm64|aarch64) ARCH="aarch64" ;;
  *) echo "unsupported arch: $arch" >&2; exit 1 ;;
esac
PLATFORM="$OS-$ARCH"
RAW_ASSET="locallmos-agent-$PLATFORM"
ASSET="$RAW_ASSET"
INSTALL_KIND="raw"

# Reject targets CI doesn't publish, so users get a clear message instead of a
# confusing 404 on download. Keep in sync with the release.yml build matrix.
case "$PLATFORM" in
  linux-x86_64|macos-aarch64) ;;
  macos-x86_64)
    echo "LocalLMOS provides Apple Silicon (arm64) macOS builds only — Intel Macs are not supported." >&2
    exit 1 ;;
  *) echo "no prebuilt agent for $PLATFORM — see the release matrix." >&2; exit 1 ;;
esac
if [ "$OS" = "macos" ] && [ "$MODE" = "desktop" ]; then
  ASSET="$RAW_ASSET.app.zip"
  INSTALL_KIND="macos_app"
fi

# GitHub's /releases/latest/download/<asset> redirects to the newest release's
# asset, so we need no API token or jq. A pinned version uses the tag path.
if [ "$VERSION" = "latest" ]; then
  BASE="https://github.com/$REPO/releases/latest/download"
else
  BASE="https://github.com/$REPO/releases/download/$VERSION"
fi

need() { command -v "$1" >/dev/null 2>&1 || { echo "missing required tool: $1" >&2; exit 1; }; }
need curl

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "==> Downloading $ASSET ($VERSION)"
if ! curl -fsSL "$BASE/$ASSET" -o "$TMP/agent"; then
  if [ "$INSTALL_KIND" = "macos_app" ]; then
    echo "!! macOS app bundle artifact not found; falling back to raw agent binary."
    ASSET="$RAW_ASSET"
    INSTALL_KIND="raw"
    curl -fsSL "$BASE/$ASSET" -o "$TMP/agent"
  else
    exit 1
  fi
fi
curl -fsSL "$BASE/$ASSET.sha256" -o "$TMP/agent.sha256"
curl -fsSL "$BASE/$ASSET.minisig" -o "$TMP/agent.minisig" || true

echo "==> Verifying checksum"
expected="$(awk '{print $1}' "$TMP/agent.sha256")"
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$TMP/agent" | awk '{print $1}')"
else
  actual="$(shasum -a 256 "$TMP/agent" | awk '{print $1}')"
fi
if [ "$expected" != "$actual" ]; then
  echo "checksum mismatch: expected $expected, got $actual" >&2
  exit 1
fi

# Signature check when minisign is available; otherwise warn (the checksum still
# gates the download, and the agent re-verifies the signature on every update).
if command -v minisign >/dev/null 2>&1 && [ -f "$TMP/agent.minisig" ]; then
  echo "==> Verifying signature"
  minisign -Vm "$TMP/agent" -P "$PUBKEY" -x "$TMP/agent.minisig"
else
  echo "!! minisign not found — skipping signature check (checksum verified)."
fi

if [ "$INSTALL_KIND" = "macos_app" ]; then
  APP_DST="/Applications/LocaLLMOS Agent.app"
  echo "==> Installing app to $APP_DST"
  need ditto
  ditto -x -k "$TMP/agent" "$TMP/app"
  APP_SRC="$TMP/app/LocaLLMOS Agent.app"
  if [ ! -d "$APP_SRC" ]; then
    echo "app bundle artifact did not contain LocaLLMOS Agent.app" >&2
    exit 1
  fi
  sudo ditto "$APP_SRC" "$APP_DST"
  sudo mkdir -p /usr/local/bin
  APP_BIN="$APP_DST/Contents/MacOS/locallmos-agent"
  if [ ! -x "$APP_BIN" ]; then
    APP_BIN="$APP_DST/Contents/MacOS/LocaLLMOS Agent"
  fi
  if [ ! -x "$APP_BIN" ]; then
    echo "could not find executable inside $APP_DST" >&2
    exit 1
  fi
  sudo ln -sf "$APP_BIN" "$BIN_DST"
else
  echo "==> Installing to $BIN_DST"
  chmod +x "$TMP/agent"
  sudo mkdir -p /usr/local/bin
  sudo install -m 0755 "$TMP/agent" "$BIN_DST"
fi

user_config_file() {
  if [ "$OS" = "macos" ]; then
    printf '%s\n' "$HOME/Library/Application Support/locallmos-agent/config.json"
  else
    printf '%s\n' "${XDG_CONFIG_HOME:-$HOME/.config}/locallmos-agent/config.json"
  fi
}

install_linux_launcher() {
  if [ "$OS" != "linux" ] || [ "$MODE" != "desktop" ]; then
    return
  fi

  app_dir="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
  icon_dir="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/128x128/apps"
  desktop_file="$app_dir/os.locallmos.agent.desktop"
  icon_file="$icon_dir/os.locallmos.agent.png"

  echo "==> Installing Linux desktop launcher"
  mkdir -p "$app_dir" "$icon_dir"
  base64_decode() {
    if base64 --help 2>&1 | grep -q -- '-d'; then
      base64 -d
    else
      base64 -D
    fi
  }
  base64_decode > "$icon_file" <<'EOF'
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
EOF

  cat > "$desktop_file" <<EOF
[Desktop Entry]
Type=Application
Name=LocalLMOS Agent
Comment=Monitor and control local LLM runtimes
Exec=${RUNTIME_ENV:+env $RUNTIME_ENV }$BIN_DST
Icon=os.locallmos.agent
Terminal=false
Categories=Utility;Network;
StartupNotify=true
StartupWMClass=LocalLMOS Agent
EOF
  chmod 0644 "$desktop_file"
  if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$app_dir" >/dev/null 2>&1 || true
  fi
  if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q -t "${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor" >/dev/null 2>&1 || true
  fi
}

launch_desktop() {
  if [ "$NO_LAUNCH" = "1" ]; then
    echo "==> Installed. Launch with: $BIN_DST"
    return
  fi
  if [ "$OS" = "linux" ] && [ -z "${DISPLAY:-}" ] && [ -z "${WAYLAND_DISPLAY:-}" ]; then
    echo "!! No Linux graphical session detected (DISPLAY/WAYLAND_DISPLAY is empty)."
    echo "   Installed. Launch from your desktop session with: $BIN_DST"
    return
  fi
  if [ "$OS" = "linux" ] && command -v ldd >/dev/null 2>&1; then
    missing="$(ldd "$BIN_DST" 2>/dev/null | awk '/not found/ {print $1}' | tr '\n' ' ')"
    if [ -n "$missing" ]; then
      echo "!! Missing desktop runtime libraries: $missing"
      echo "   Install your distro's WebKitGTK/GTK/AppIndicator packages, then run: $BIN_DST"
      return
    fi
  fi
  if [ "$OS" = "macos" ] && [ -d "/Applications/LocaLLMOS Agent.app" ]; then
    echo "==> Launching LocalLMOS Agent"
    open "/Applications/LocaLLMOS Agent.app"
    sleep 2
    echo "==> Done. LocalLMOS Agent is running in your desktop session."
    return
  fi

  echo "==> Launching LocalLMOS Agent"
  # shellcheck disable=SC2086 # RUNTIME_ENV is intentionally word-split into env args
  env LOCALLMOS_SUPABASE_URL="$SUPABASE_URL" \
      LOCALLMOS_SUPABASE_ANON_KEY="$ANON_KEY" \
      $RUNTIME_ENV \
      nohup "$BIN_DST" >/dev/null 2>&1 &
  pid=$!
  sleep 2
  if kill -0 "$pid" 2>/dev/null; then
    echo "==> Done. LocalLMOS Agent is running in your desktop session."
  else
    echo "!! Installed, but the desktop app exited during launch."
    echo "   Run this to see the error: $BIN_DST"
  fi
}

desktop_service_notice() {
  if [ "$OS" = "linux" ] && [ -f /etc/systemd/system/locallmos-agent.service ]; then
    echo "!! A headless systemd service is already installed."
    echo "   Keep it for server mode, or remove it with: sudo systemctl disable --now locallmos-agent"
  elif [ "$OS" = "macos" ] && [ -f /Library/LaunchDaemons/os.locallmos.agent.plist ]; then
    echo "!! A headless launchd daemon is already installed."
    echo "   Keep it for server mode, or remove it with: sudo launchctl unload -w /Library/LaunchDaemons/os.locallmos.agent.plist"
  fi
}

# ---- llama.cpp provisioning ------------------------------------------------
# Provisioning lives in service/lib-llamacpp.sh, shared with install-service.sh
# so they can't drift. From a checkout it's beside this script; when piped via
# `curl | sh` there's no local copy, so fetch it from the repo.
if [ "$RUNTIME" = "llamacpp" ]; then
  _here="$(CDPATH= cd -- "$(dirname -- "$0")" 2>/dev/null && pwd)"
  if [ -n "$_here" ] && [ -f "$_here/lib-llamacpp.sh" ]; then
    . "$_here/lib-llamacpp.sh"
  else
    LIB_REF="${LOCALLMOS_LIB_REF:-main}"
    echo "==> Fetching llama.cpp provisioning helper ($REPO@$LIB_REF)"
    curl -fsSL "https://raw.githubusercontent.com/$REPO/$LIB_REF/service/lib-llamacpp.sh" \
      -o "$TMP/lib-llamacpp.sh" \
      || { echo "could not fetch lib-llamacpp.sh" >&2; exit 1; }
    . "$TMP/lib-llamacpp.sh"
  fi
fi

# Provision before the mode-specific install so the env is ready for both the
# systemd unit (agent.env) and the desktop launch. RUNTIME_ENV is the launch-time
# env prefix used by desktop mode.
RUNTIME_ENV=""
if [ "$RUNTIME" = "llamacpp" ]; then
  provision_llamacpp
  RUNTIME_ENV="LOCALLMOS_RUNTIME=llamacpp LOCALLMOS_LLAMACPP_BIN=$LLAMA_BIN LOCALLMOS_LLAMACPP_MODELS_DIR=$MODELS_DIR"
fi

# ---- desktop install -------------------------------------------------------
if [ "$MODE" = "desktop" ]; then
  desktop_service_notice
  USER_CONFIG="$(user_config_file)"
  if [ -f "$USER_CONFIG" ] && grep -q '"refresh_secret"' "$USER_CONFIG"; then
    echo "==> Already enrolled — skipping enrollment"
  elif [ -n "$CODE" ]; then
    echo "==> Enrolling desktop app as '$NAME'"
    env LOCALLMOS_SUPABASE_URL="$SUPABASE_URL" \
        LOCALLMOS_SUPABASE_ANON_KEY="$ANON_KEY" \
        "$BIN_DST" enroll --code "$CODE" --name "$NAME"
  else
    echo "==> No --code given. Opening the tray app for local mode and pairing."
  fi
  install_linux_launcher
  launch_desktop
else
  echo "==> Writing $CONFIG_DIR/agent.env"
  sudo mkdir -p "$CONFIG_DIR"
  if [ ! -f "$CONFIG_DIR/agent.env" ]; then
    sudo tee "$CONFIG_DIR/agent.env" >/dev/null <<EOF
LOCALLMOS_SUPABASE_URL=$SUPABASE_URL
LOCALLMOS_SUPABASE_ANON_KEY=$ANON_KEY
EOF
    sudo chmod 0600 "$CONFIG_DIR/agent.env"
  fi
  # Point the service at the provisioned llama.cpp engine (idempotent).
  if [ "$RUNTIME" = "llamacpp" ] \
    && ! sudo grep -q '^LOCALLMOS_RUNTIME=' "$CONFIG_DIR/agent.env" 2>/dev/null; then
    sudo tee -a "$CONFIG_DIR/agent.env" >/dev/null <<EOF
LOCALLMOS_RUNTIME=llamacpp
LOCALLMOS_LLAMACPP_BIN=$LLAMA_BIN
LOCALLMOS_LLAMACPP_MODELS_DIR=$MODELS_DIR
EOF
  fi

  # ---- service install -----------------------------------------------------
  SERVICE_READY=0
  if [ "$OS" = "linux" ]; then
    echo "==> Installing systemd unit"
    sudo tee /etc/systemd/system/locallmos-agent.service >/dev/null <<'EOF'
[Unit]
Description=LocalLMOS Agent (local LLM rig monitor/controller)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/locallmos-agent service
# `always`: self-update exits cleanly and relies on the restart to relaunch on
# the new binary.
Restart=always
RestartSec=5
Environment=LOCALLMOS_CONFIG_DIR=/etc/locallmos-agent
EnvironmentFile=-/etc/locallmos-agent/agent.env
User=root

[Install]
WantedBy=multi-user.target
EOF
    sudo systemctl daemon-reload
  else
    echo "==> Installing launchd daemon"
    sudo tee /Library/LaunchDaemons/os.locallmos.agent.plist >/dev/null <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>os.locallmos.agent</string>
  <key>ProgramArguments</key><array>
    <string>/usr/local/bin/locallmos-agent</string><string>service</string>
  </array>
  <key>EnvironmentVariables</key><dict>
    <key>LOCALLMOS_CONFIG_DIR</key><string>/etc/locallmos-agent</string>
    <key>LOCALLMOS_SUPABASE_URL</key><string>$SUPABASE_URL</string>
    <key>LOCALLMOS_SUPABASE_ANON_KEY</key><string>$ANON_KEY</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>/var/log/locallmos-agent.log</string>
  <key>StandardErrorPath</key><string>/var/log/locallmos-agent.log</string>
</dict></plist>
EOF
  fi

  # ---- enroll --------------------------------------------------------------
  if sudo test -f "$CONFIG_DIR/config.json" && sudo grep -q '"refresh_secret"' "$CONFIG_DIR/config.json"; then
    echo "==> Already enrolled — skipping enrollment"
    SERVICE_READY=1
  elif [ -n "$CODE" ]; then
    echo "==> Enrolling as '$NAME'"
    sudo env LOCALLMOS_CONFIG_DIR="$CONFIG_DIR" \
      LOCALLMOS_SUPABASE_URL="$SUPABASE_URL" \
      LOCALLMOS_SUPABASE_ANON_KEY="$ANON_KEY" \
      "$BIN_DST" enroll --code "$CODE" --name "$NAME"
    SERVICE_READY=1
  else
    echo "!! No --code given. Generate a pairing code in the dashboard, then run:"
    echo "   sudo env LOCALLMOS_CONFIG_DIR=$CONFIG_DIR LOCALLMOS_SUPABASE_URL=$SUPABASE_URL LOCALLMOS_SUPABASE_ANON_KEY=<ANON_KEY> $BIN_DST enroll --code <CODE> --name '$NAME'"
  fi

  # ---- start ---------------------------------------------------------------
  if [ "$SERVICE_READY" = "1" ]; then
    if [ "$OS" = "linux" ]; then
      sudo systemctl enable --now locallmos-agent
      echo "==> Done. Service logs: journalctl -u locallmos-agent -f"
    else
      sudo launchctl unload -w /Library/LaunchDaemons/os.locallmos.agent.plist 2>/dev/null || true
      sudo launchctl load -w /Library/LaunchDaemons/os.locallmos.agent.plist
      echo "==> Done. Service logs: tail -f /var/log/locallmos-agent.log"
    fi
  else
    echo "==> Service installed but not started because this rig is not enrolled."
  fi
fi

# ---- runtime check ---------------------------------------------------------
if [ "$RUNTIME" = "llamacpp" ]; then
  echo
  echo "==> Runtime: llama.cpp (llama-server) — $LLAMA_BIN"
  echo "   Add a .gguf to $MODELS_DIR, then select it in the dashboard."
elif ! command -v ollama >/dev/null 2>&1; then
  echo
  echo "!! Ollama was not detected on this machine."
  echo "   LocalLMOS uses Ollama to run models locally. Install it:"
  if [ "$OS" = "macos" ]; then
    echo "     Download the app from https://ollama.com/download"
    echo "     (or, with Homebrew:  brew install ollama)"
  else
    echo "     curl -fsSL https://ollama.com/install.sh | sh"
  fi
  echo "   Then pull a model, e.g.:  ollama pull llama3.2"
fi
