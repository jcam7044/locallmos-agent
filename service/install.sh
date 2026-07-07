#!/bin/sh
# LocalLMOS agent installer (Linux + macOS). POSIX sh — runs under dash when
# invoked as `curl … | sh` (the shebang is ignored in that case anyway).
#
#   curl -fsSL https://get.locallmos.os/install.sh | sh -s -- \
#     --supabase-url https://<ref>.supabase.co --anon-key <ANON> \
#     --code <PAIRING_CODE> --name "My Rig"
#
# Downloads a prebuilt, signed agent binary from GitHub Releases, verifies it,
# installs it to /usr/local/bin, sets up the service (systemd on Linux, launchd
# on macOS), and enrolls the rig. Ongoing updates are handled by the agent
# itself (self-update), so there is no package manager to keep in sync.
# POSIX sh only: no `pipefail` (dash lacks it); `-e` still aborts on the `curl -f`
# download failures and on a checksum mismatch.
set -eu

# ---- defaults (override via flags or env) ---------------------------------
REPO="${LOCALLMOS_REPO:-jcam7044/locallmos}"     # GitHub owner/repo hosting releases
CHANNEL="${LOCALLMOS_CHANNEL:-stable}"
VERSION="latest"                                  # or an explicit vX.Y.Z tag
NAME="$(hostname 2>/dev/null || echo my-rig)"
CODE=""
SUPABASE_URL="${LOCALLMOS_SUPABASE_URL:-}"
ANON_KEY="${LOCALLMOS_SUPABASE_ANON_KEY:-}"

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
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

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
ASSET="locallmos-agent-$PLATFORM"

# Reject targets CI doesn't publish, so users get a clear message instead of a
# confusing 404 on download. Keep in sync with the release.yml build matrix.
case "$PLATFORM" in
  linux-x86_64|macos-aarch64) ;;
  macos-x86_64)
    echo "LocalLMOS provides Apple Silicon (arm64) macOS builds only — Intel Macs are not supported." >&2
    exit 1 ;;
  *) echo "no prebuilt agent for $PLATFORM — see the release matrix." >&2; exit 1 ;;
esac

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
curl -fsSL "$BASE/$ASSET" -o "$TMP/agent"
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

echo "==> Installing to $BIN_DST"
chmod +x "$TMP/agent"
sudo install -m 0755 "$TMP/agent" "$BIN_DST"

echo "==> Writing $CONFIG_DIR/agent.env"
sudo mkdir -p "$CONFIG_DIR"
if [ ! -f "$CONFIG_DIR/agent.env" ]; then
  sudo tee "$CONFIG_DIR/agent.env" >/dev/null <<EOF
LOCALLMOS_SUPABASE_URL=$SUPABASE_URL
LOCALLMOS_SUPABASE_ANON_KEY=$ANON_KEY
EOF
  sudo chmod 0600 "$CONFIG_DIR/agent.env"
fi

# ---- service install -------------------------------------------------------
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

# ---- enroll ----------------------------------------------------------------
if sudo test -f "$CONFIG_DIR/config.json" && sudo grep -q '"refresh_secret"' "$CONFIG_DIR/config.json"; then
  echo "==> Already enrolled — skipping enrollment"
elif [ -n "$CODE" ]; then
  echo "==> Enrolling as '$NAME'"
  sudo env LOCALLMOS_CONFIG_DIR="$CONFIG_DIR" \
    sh -c "set -a; . '$CONFIG_DIR/agent.env'; set +a; '$BIN_DST' enroll --code '$CODE' --name '$NAME'"
else
  echo "!! No --code given. Generate a pairing code in the dashboard, then run:"
  echo "   sudo env LOCALLMOS_CONFIG_DIR=$CONFIG_DIR $BIN_DST enroll --code <CODE> --name '$NAME'"
fi

# ---- start -----------------------------------------------------------------
if [ "$OS" = "linux" ]; then
  sudo systemctl enable --now locallmos-agent
  echo "==> Done. Logs: journalctl -u locallmos-agent -f"
else
  sudo launchctl unload -w /Library/LaunchDaemons/os.locallmos.agent.plist 2>/dev/null || true
  sudo launchctl load -w /Library/LaunchDaemons/os.locallmos.agent.plist
  echo "==> Done. Logs: tail -f /var/log/locallmos-agent.log"
fi

# ---- runtime check ---------------------------------------------------------
if ! command -v ollama >/dev/null 2>&1; then
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
