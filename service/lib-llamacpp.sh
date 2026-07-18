# Shared llama.cpp provisioning, sourced by both install.sh (public curl|sh
# installer) and install-service.sh (from-source dev/service installer) so the
# two can't drift. POSIX sh — must parse under dash.
#
# The caller must set these globals before calling provision_llamacpp:
#   OS               "linux" | "macos"
#   ARCH             "x86_64" | "aarch64"
#   MODE             "service" | "desktop"   (install dir + sudo depend on it)
#   LLAMACPP_REPO    e.g. "ggml-org/llama.cpp"
#   LLAMACPP_VERSION a release tag (e.g. "b10068") or "latest"
# On success it sets: LLAMA_BIN (path to llama-server) and MODELS_DIR.

_llamacpp_need() {
  command -v "$1" >/dev/null 2>&1 || { echo "missing required tool: $1" >&2; exit 1; }
}

# Resolve the release tag ("latest" → newest tag via the GitHub API; else as-is).
resolve_llamacpp_tag() {
  if [ "$LLAMACPP_VERSION" != "latest" ]; then
    printf '%s\n' "$LLAMACPP_VERSION"
    return
  fi
  curl -fsSL "https://api.github.com/repos/$LLAMACPP_REPO/releases/latest" 2>/dev/null \
    | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1
}

# Ordered candidate release assets (accelerated → CPU) for this OS/arch. On Linux
# the accelerated build is Vulkan (llama.cpp ships no prebuilt Linux CUDA), which
# covers NVIDIA/AMD/Intel; macOS arm64 has Metal baked in. Assets are .tar.gz.
llamacpp_assets() {
  tag="$1"
  case "$OS" in
    macos)
      case "$ARCH" in
        aarch64) printf 'llama-%s-bin-macos-arm64.tar.gz\n' "$tag" ;;
        x86_64)  printf 'llama-%s-bin-macos-x64.tar.gz\n' "$tag" ;;
      esac
      ;;
    linux)
      accel=""
      if command -v nvidia-smi >/dev/null 2>&1 || [ -e /dev/dri ] \
        || command -v vulkaninfo >/dev/null 2>&1; then
        accel="yes"
      fi
      case "$ARCH" in
        x86_64)
          [ -n "$accel" ] && printf 'llama-%s-bin-ubuntu-vulkan-x64.tar.gz\n' "$tag"
          printf 'llama-%s-bin-ubuntu-x64.tar.gz\n' "$tag"
          ;;
        aarch64)
          [ -n "$accel" ] && printf 'llama-%s-bin-ubuntu-vulkan-arm64.tar.gz\n' "$tag"
          printf 'llama-%s-bin-ubuntu-arm64.tar.gz\n' "$tag"
          ;;
      esac
      ;;
  esac
}

# Download + install a prebuilt llama-server for this rig. Idempotent: reuses an
# existing install. Sets the globals LLAMA_BIN and MODELS_DIR; the caller writes
# the agent env once its config file exists (timing/location differ per installer).
provision_llamacpp() {
  _llamacpp_need curl
  if [ "$MODE" = "service" ]; then
    LLAMA_DIR="/opt/locallmos/llama"
    MODELS_DIR="/var/lib/locallmos/models"
    SUDO="sudo"
  else
    LLAMA_DIR="$HOME/.local/opt/locallmos/llama"
    MODELS_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/locallmos/models"
    SUDO=""
  fi

  existing="$(find "$LLAMA_DIR" -type f -name llama-server 2>/dev/null | head -n1)"
  if [ -n "$existing" ]; then
    echo "==> llama-server already provisioned ($existing)"
    LLAMA_BIN="$existing"
  else
    _llamacpp_need tar
    tag="$(resolve_llamacpp_tag)"
    [ -n "$tag" ] || { echo "could not resolve llama.cpp version" >&2; exit 1; }
    echo "==> Provisioning llama.cpp ($tag) for $OS-$ARCH"
    LTMP="$(mktemp -d)"
    got=""
    for asset in $(llamacpp_assets "$tag"); do
      url="https://github.com/$LLAMACPP_REPO/releases/download/$tag/$asset"
      echo "    trying $asset"
      if curl -fsSL "$url" -o "$LTMP/llama.tgz"; then got="$asset"; break; fi
    done
    if [ -z "$got" ]; then
      echo "no prebuilt llama-server for $OS-$ARCH at $tag" >&2
      rm -rf "$LTMP"; exit 1
    fi
    echo "==> Extracting $got"
    mkdir -p "$LTMP/x"
    tar -xzf "$LTMP/llama.tgz" -C "$LTMP/x"
    if [ -z "$(find "$LTMP/x" -type f -name llama-server 2>/dev/null | head -n1)" ]; then
      echo "llama-server not found in $got" >&2
      rm -rf "$LTMP"; exit 1
    fi
    # Copy the extracted tree whole so llama-server keeps its co-located shared
    # libraries (the binaries resolve them via an $ORIGIN-relative rpath).
    $SUDO mkdir -p "$LLAMA_DIR"
    $SUDO cp -a "$LTMP/x/." "$LLAMA_DIR/"
    rm -rf "$LTMP"
    LLAMA_BIN="$(find "$LLAMA_DIR" -type f -name llama-server 2>/dev/null | head -n1)"
    $SUDO chmod +x "$LLAMA_BIN"
  fi

  if ! "$LLAMA_BIN" --version >/dev/null 2>&1; then
    echo "!! llama-server did not run a smoke test (missing GPU/driver libs?);"
    echo "   it may still work at chat time, or reinstall with a CPU build via"
    echo "   LOCALLMOS_LLAMACPP_VERSION and no GPU present."
  fi

  $SUDO mkdir -p "$MODELS_DIR"
  echo "==> llama.cpp models dir: $MODELS_DIR  (drop your .gguf files here)"
}
