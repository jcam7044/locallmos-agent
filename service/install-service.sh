#!/usr/bin/env bash
# Install the LocalLMOS agent as a systemd system service (Linux).
#
#   ./install-service.sh --code <PAIRING_CODE> --name "<RIG_NAME>"
#
# Steps: build a release binary, install it + the unit + config, enroll (if a
# code is given and the rig isn't enrolled yet), then enable + start the service.
# Re-runnable: skips enrollment if already enrolled.
set -euo pipefail

CODE=""
NAME="$(hostname)"
# Runtime selection, shared with install.sh (see service/lib-llamacpp.sh).
MODE="service"
RUNTIME="${LOCALLMOS_RUNTIME:-ollama}"
LLAMACPP_REPO="${LOCALLMOS_LLAMACPP_REPO:-ggml-org/llama.cpp}"           # vulkan/rocm/cpu/metal source
LLAMACPP_CUDA_REPO="${LOCALLMOS_LLAMACPP_CUDA_REPO:-jcam7044/locallmos-agent}" # self-hosted Linux CUDA prebuilts
LLAMACPP_VERSION="${LOCALLMOS_LLAMACPP_VERSION:-}"       # empty = resolve from the LLAMACPP_VERSION manifest
LLAMACPP_VERSION_FALLBACK="b10087"                       # offline safety net only
LLAMACPP_BACKEND="${LOCALLMOS_LLAMACPP_BACKEND:-auto}"   # auto|cuda|rocm|vulkan|cpu|metal (forced = no fallback)
while [[ $# -gt 0 ]]; do
  case "$1" in
    --code) CODE="$2"; shift 2 ;;
    --name) NAME="$2"; shift 2 ;;
    --runtime) RUNTIME="$2"; shift 2 ;;
    --llamacpp-version) LLAMACPP_VERSION="$2"; shift 2 ;;
    --llamacpp-backend) LLAMACPP_BACKEND="$2"; shift 2 ;;
    *) echo "unknown arg: $1"; exit 2 ;;
  esac
done
case "$RUNTIME" in
  ollama|llamacpp) ;;
  *) echo "unknown runtime: $RUNTIME (expected ollama or llamacpp)"; exit 2 ;;
esac
case "$LLAMACPP_BACKEND" in
  auto|cuda|rocm|vulkan|cpu|metal) ;;
  *) echo "unknown llamacpp backend: $LLAMACPP_BACKEND (expected auto|cuda|rocm|vulkan|cpu|metal)"; exit 2 ;;
esac

# Platform detection for the shared provisioning lib ("{os}"/"{arch}" values).
case "$(uname -s)" in
  Linux)  OS="linux" ;;
  Darwin) OS="macos" ;;
  *) OS="linux" ;;
esac
case "$(uname -m)" in
  x86_64|amd64)  ARCH="x86_64" ;;
  arm64|aarch64) ARCH="aarch64" ;;
  *) ARCH="x86_64" ;;
esac

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib-llamacpp.sh
. "$HERE/lib-llamacpp.sh"
SRC_TAURI="$(cd "$HERE/../src-tauri" && pwd)"
AGENT_DIR="$(cd "$HERE/.." && pwd)"
CONFIG_DIR="/etc/locallmos-agent"
BIN_DST="/usr/local/bin/locallmos-agent"
UNIT_DST="/etc/systemd/system/locallmos-agent.service"

# The release build embeds the built frontend; build.rs hard-fails without
# ../dist/index.html. Build it first (the public install.sh path skips this — it
# downloads a prebuilt binary whose frontend was built in CI).
echo "==> Building frontend (dist/)"
if ! command -v pnpm >/dev/null 2>&1; then
  echo "!! pnpm not found on PATH — the release build needs the frontend built first." >&2
  echo "   Install it, e.g.:  corepack enable && corepack prepare pnpm@9.15.9 --activate" >&2
  echo "   or:                npm install -g pnpm" >&2
  exit 1
fi
( cd "$AGENT_DIR" && pnpm install && pnpm build )

echo "==> Building release binary (this can take a few minutes the first time)"
( cd "$SRC_TAURI" && cargo build --release )
BIN_SRC="$SRC_TAURI/target/release/locallmos-agent"

echo "==> Installing binary to $BIN_DST"
sudo install -m 0755 "$BIN_SRC" "$BIN_DST"

echo "==> Setting up $CONFIG_DIR"
sudo mkdir -p "$CONFIG_DIR"
if [[ ! -f "$CONFIG_DIR/agent.env" ]]; then
  # Prefer an existing local .env, else fall back to the template.
  if [[ -f "$AGENT_DIR/.env" ]]; then
    sudo cp "$AGENT_DIR/.env" "$CONFIG_DIR/agent.env"
  else
    sudo cp "$HERE/agent.env.example" "$CONFIG_DIR/agent.env"
  fi
  sudo chmod 0600 "$CONFIG_DIR/agent.env"
  echo "    wrote $CONFIG_DIR/agent.env — verify LOCALLMOS_SUPABASE_URL / _ANON_KEY"
fi

# Provision llama-server and point the service at it. Rewrite the runtime keys
# (rather than append-if-absent) so a reprovision to a new bin/backend can't leave
# stale values behind.
if [[ "$RUNTIME" == "llamacpp" ]]; then
  # Fill the version from the local manifest (then fallback) unless set explicitly.
  if [[ -z "$LLAMACPP_VERSION" ]]; then
    LLAMACPP_VERSION="$(_llamacpp_parse_version < "$HERE/LLAMACPP_VERSION" 2>/dev/null || true)"
    [[ -n "$LLAMACPP_VERSION" ]] || LLAMACPP_VERSION="$LLAMACPP_VERSION_FALLBACK"
    echo "==> llama.cpp version $LLAMACPP_VERSION"
  fi
  provision_llamacpp
  NEW_ENV="$(mktemp)"
  # Read the root-owned 0600 agent.env via sudo; the redirect writes the temp as
  # the invoking user (that's intended, not a sudo-redirect bug).
  # shellcheck disable=SC2024
  sudo grep -v -E '^LOCALLMOS_(RUNTIME|LLAMACPP_BIN|LLAMACPP_MODELS_DIR|LLAMACPP_BACKEND)=' \
    "$CONFIG_DIR/agent.env" 2>/dev/null > "$NEW_ENV" || true
  {
    echo "LOCALLMOS_RUNTIME=llamacpp"
    echo "LOCALLMOS_LLAMACPP_BIN=$LLAMA_BIN"
    echo "LOCALLMOS_LLAMACPP_MODELS_DIR=$MODELS_DIR"
    echo "LOCALLMOS_LLAMACPP_BACKEND=$LLAMA_BACKEND"
  } >> "$NEW_ENV"
  sudo cp "$NEW_ENV" "$CONFIG_DIR/agent.env"
  sudo chmod 0600 "$CONFIG_DIR/agent.env"
  rm -f "$NEW_ENV"
fi

echo "==> Installing systemd unit"
sudo cp "$HERE/locallmos-agent.service" "$UNIT_DST"
sudo systemctl daemon-reload

# Enroll if needed (config.json lives in $CONFIG_DIR).
if sudo test -f "$CONFIG_DIR/config.json" && sudo grep -q '"refresh_secret"' "$CONFIG_DIR/config.json"; then
  echo "==> Already enrolled — skipping enrollment"
elif [[ -n "$CODE" ]]; then
  echo "==> Enrolling as '$NAME'"
  sudo env LOCALLMOS_CONFIG_DIR="$CONFIG_DIR" \
    bash -c "set -a; source '$CONFIG_DIR/agent.env'; set +a; '$BIN_DST' enroll --code '$CODE' --name '$NAME'"
else
  echo "!! Not enrolled and no --code given. Generate a code in the dashboard, then:"
  echo "   sudo env LOCALLMOS_CONFIG_DIR=$CONFIG_DIR bash -c \"set -a; source $CONFIG_DIR/agent.env; set +a; $BIN_DST enroll --code <CODE> --name '$NAME'\""
fi

echo "==> Enabling + starting service"
sudo systemctl enable --now locallmos-agent

echo "==> Done. Check status with:"
echo "   systemctl status locallmos-agent"
echo "   journalctl -u locallmos-agent -f"
