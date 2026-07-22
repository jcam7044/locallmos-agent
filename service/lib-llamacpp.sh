# Shared llama.cpp provisioning, sourced by both install.sh (public curl|sh
# installer) and install-service.sh (from-source dev/service installer) so the
# two can't drift. POSIX sh — must parse under dash.
#
# The caller must set these globals before calling provision_llamacpp:
#   OS                "linux" | "macos"
#   ARCH              "x86_64" | "aarch64"
#   MODE              "service" | "desktop"   (install dir + sudo depend on it)
#   LLAMACPP_REPO     e.g. "ggml-org/llama.cpp"  (source of vulkan/rocm/cpu/metal)
#   LLAMACPP_VERSION  a release tag (e.g. "b10068") or "latest"
# Optional (safe defaults so already-shipped install.sh copies keep working):
#   LLAMACPP_BACKEND  "auto" (default) | cuda | rocm | vulkan | cpu | metal
#   LLAMACPP_CUDA_REPO  owner/repo hosting the self-hosted Linux CUDA prebuilts
#                       (default "jcam7044/locallmos-agent"; upstream ships none)
# On success it sets: LLAMA_BIN (path to llama-server), MODELS_DIR, LLAMA_BACKEND.

_llamacpp_need() {
  command -v "$1" >/dev/null 2>&1 || { echo "missing required tool: $1" >&2; exit 1; }
}

# Parse a version-manifest file (service/LLAMACPP_VERSION) from stdin: echo the
# first non-blank, non-comment line, whitespace-stripped. Empty if none. awk (not
# grep) so it always exits 0 — safe under `set -e`/`pipefail` in either installer.
_llamacpp_parse_version() {
  awk 'NF && $1 !~ /^#/ { gsub(/[ \t\r]/, "", $0); print; exit }'
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

# ---- hardware detection ----------------------------------------------------
# iGPU policy: an integrated GPU must NOT steer backend choice (e.g. a small AMD
# iGPU next to NVIDIA dGPUs must not drag the machine toward Vulkan/ROCm). A drm
# card of the given PCI vendor id qualifies only if it is discrete-class — either
# it advertises >= 4 GiB of dedicated VRAM (mem_info_vram_total, an amdgpu sysfs
# attribute) OR its PCI device id is on the unified-memory APU whitelist (Strix
# Halo 0x1586 / gfx1151). The whitelist is meant to grow.
_llx_qualifying_gpu() {
  _want="$1"
  _min_vram=4294967296 # 4 GiB in bytes (literal to avoid 32-bit multiply overflow)
  for _d in /sys/class/drm/card*/device; do
    [ -d "$_d" ] || continue
    [ -f "$_d/vendor" ] || continue
    read -r _v < "$_d/vendor" 2>/dev/null || continue
    [ "$_v" = "$_want" ] || continue
    if [ -f "$_d/mem_info_vram_total" ]; then
      read -r _vram < "$_d/mem_info_vram_total" 2>/dev/null || _vram=0
      case "$_vram" in ''|*[!0-9]*) _vram=0 ;; esac
      [ "$_vram" -ge "$_min_vram" ] && return 0
    fi
    if [ -f "$_d/device" ]; then
      read -r _did < "$_d/device" 2>/dev/null || _did=""
      case "$_did" in
        0x1586) return 0 ;; # Strix Halo (gfx1151), unified memory
      esac
    fi
  done
  return 1
}

# Echo the hosted CUDA prebuilt variant this rig's NVIDIA driver can load, from
# the CUDA version nvidia-smi reports (the max the driver supports): >= 13.0 →
# "13.3" (Blackwell-capable), >= 12.4 → "12.4", else empty (driver too old /
# unreadable → caller falls back to vulkan). Mirrors the Windows 12.4/13.3 split.
_llx_cuda_variant() {
  _ver="$(nvidia-smi 2>/dev/null \
    | sed -n 's/.*CUDA Version: *\([0-9][0-9]*\.[0-9][0-9]*\).*/\1/p' | head -n1)"
  [ -n "$_ver" ] || return 0
  _maj="${_ver%%.*}"
  _min="${_ver#*.}"
  case "$_maj" in ''|*[!0-9]*) return 0 ;; esac
  case "$_min" in ''|*[!0-9]*) _min=0 ;; esac
  if [ "$_maj" -ge 13 ]; then
    printf '13.3\n'
  elif [ "$_maj" -eq 12 ] && [ "$_min" -ge 4 ]; then
    printf '12.4\n'
  fi
  # anything older than 12.4 → print nothing
}

# Echo exactly one backend word for this rig. Logs go to stderr; stdout is the
# return channel. NVIDIA is checked before AMD (mixed rigs prefer CUDA; the
# agent's pick_devices handles device choice at run time). Apple Silicon is
# handled by the macOS→metal branch; DGX Spark routes down NVIDIA via nvidia-smi.
llamacpp_detect_backend() {
  if [ "$OS" = "macos" ]; then
    printf 'metal\n'
    return
  fi
  if nvidia-smi -L >/dev/null 2>&1; then
    if [ -n "$(_llx_cuda_variant)" ]; then
      printf 'cuda\n'
    else
      echo "   NVIDIA GPU present but the driver is too old for a hosted CUDA build → vulkan" >&2
      printf 'vulkan\n'
    fi
    return
  fi
  if _llx_qualifying_gpu 0x1002; then
    if [ "$ARCH" = "x86_64" ] && [ -e /dev/kfd ] \
      && { command -v rocminfo >/dev/null 2>&1 || command -v rocm-smi >/dev/null 2>&1 || [ -d /opt/rocm ]; }; then
      printf 'rocm\n'
    else
      printf 'vulkan\n'
    fi
    return
  fi
  # Any other qualifying discrete card (e.g. Intel Arc, or an NVIDIA card whose
  # nvidia-smi isn't working) → vulkan. Best-effort: the VRAM heuristic is amdgpu-
  # specific, so a discrete Intel/NVIDIA card that doesn't expose it falls to cpu.
  if _llx_qualifying_gpu 0x8086 || _llx_qualifying_gpu 0x10de; then
    printf 'vulkan\n'
    return
  fi
  printf 'cpu\n'
}

# Space-separated fallback chain for a target backend (POSIX has no arrays).
llamacpp_backend_chain() {
  case "$1" in
    cuda)   printf 'cuda vulkan cpu\n' ;;
    rocm)   printf 'rocm vulkan cpu\n' ;;
    vulkan) printf 'vulkan cpu\n' ;;
    cpu)    printf 'cpu\n' ;;
    metal)  printf 'metal\n' ;;
    *)      printf 'cpu\n' ;;
  esac
}

# GitHub owner/repo to download a backend's asset from. CUDA lives in the self-
# hosted builds repo (upstream ships no Linux CUDA); everything else upstream. If
# a future release mirrors all backends into the self-hosted repo, point
# LLAMACPP_REPO at it and this collapses to one source.
_llx_asset_repo() {
  case "$1" in
    cuda) printf '%s\n' "${LLAMACPP_CUDA_REPO:-jcam7044/locallmos-agent}" ;;
    *)    printf '%s\n' "$LLAMACPP_REPO" ;;
  esac
}

# Asset filename for <backend> at <tag> on this OS/ARCH. Empty when unavailable.
llamacpp_asset_for() {
  _b="$1"; _tag="$2"
  if [ "$OS" = "macos" ]; then
    case "$ARCH" in
      aarch64) printf 'llama-%s-bin-macos-arm64.tar.gz\n' "$_tag" ;;
      x86_64)  printf 'llama-%s-bin-macos-x64.tar.gz\n' "$_tag" ;;
    esac
    return
  fi
  case "$_b" in
    cuda)
      [ "$ARCH" = "x86_64" ] || return 0
      _cv="$(_llx_cuda_variant)"
      [ -n "$_cv" ] || return 0
      printf 'llama-%s-bin-ubuntu-cuda-%s-x64.tar.gz\n' "$_tag" "$_cv" ;;
    rocm)
      # "7.2" tracks upstream's ROCm asset — revisit when bumping the pinned tag.
      [ "$ARCH" = "x86_64" ] || return 0
      printf 'llama-%s-bin-ubuntu-rocm-7.2-x64.tar.gz\n' "$_tag" ;;
    vulkan)
      case "$ARCH" in
        x86_64)  printf 'llama-%s-bin-ubuntu-vulkan-x64.tar.gz\n' "$_tag" ;;
        aarch64) printf 'llama-%s-bin-ubuntu-vulkan-arm64.tar.gz\n' "$_tag" ;;
      esac ;;
    cpu)
      case "$ARCH" in
        x86_64)  printf 'llama-%s-bin-ubuntu-x64.tar.gz\n' "$_tag" ;;
        aarch64) printf 'llama-%s-bin-ubuntu-arm64.tar.gz\n' "$_tag" ;;
      esac ;;
  esac
}

# Download + extract <backend>'s prebuilt for <tag> into <stagedir>. Nonzero on
# any failure (no asset for this rig, download error, or no llama-server inside).
# The tarball tree is extracted whole so llama-server keeps its co-located shared
# libraries (resolved via an $ORIGIN-relative rpath — incl. the bundled CUDA
# runtime libs for the self-hosted cuda builds).
llamacpp_stage_prebuilt() {
  _sb="$1"; _stag="$2"; _sdir="$3"
  _asset="$(llamacpp_asset_for "$_sb" "$_stag")"
  if [ -z "$_asset" ]; then
    echo "   no $_sb asset for $OS-$ARCH at $_stag" >&2
    return 1
  fi
  _repo="$(_llx_asset_repo "$_sb")"
  _url="https://github.com/$_repo/releases/download/$_stag/$_asset"
  echo "   downloading $_asset  ($_repo)"
  if ! curl -fsSL "$_url" -o "$_sdir/.llama.tgz"; then
    echo "   download failed: $_url" >&2
    return 1
  fi
  if ! tar -xzf "$_sdir/.llama.tgz" -C "$_sdir"; then
    echo "   extract failed: $_asset" >&2
    return 1
  fi
  rm -f "$_sdir/.llama.tgz"
  if [ -z "$(find "$_sdir" -type f -name llama-server 2>/dev/null | head -n1)" ]; then
    echo "   llama-server not found in $_asset" >&2
    return 1
  fi
  return 0
}

# Locate llama-server under <dir>, make it executable, run --version. Returns its
# status — nonzero when the binary can't actually run here (missing GPU/driver
# libs), which is how the provision loop rejects a backend before committing it.
llamacpp_smoke_test() {
  _tbin="$(find "$1" -type f -name llama-server 2>/dev/null | head -n1)"
  [ -n "$_tbin" ] || return 1
  chmod +x "$_tbin" 2>/dev/null || true
  "$_tbin" --version >/dev/null 2>&1
}

# Provision a hardware-appropriate llama-server for this rig, with a staged smoke-
# tested fallback chain. Idempotent via a marker file. Sets LLAMA_BIN, MODELS_DIR
# and LLAMA_BACKEND; the caller writes the agent env once its config file exists
# (timing/location differ per installer).
provision_llamacpp() {
  _llamacpp_need curl
  _llamacpp_need tar

  _backend="${LLAMACPP_BACKEND:-auto}"
  case "$_backend" in
    auto|cuda|rocm|vulkan|cpu|metal) ;;
    *) echo "unknown LLAMACPP_BACKEND: $_backend (auto|cuda|rocm|vulkan|cpu|metal)" >&2; exit 2 ;;
  esac

  if [ "$MODE" = "service" ]; then
    LLAMA_DIR="/opt/locallmos/llama"
    MODELS_DIR="/var/lib/locallmos/models"
    SUDO="sudo"
  else
    LLAMA_DIR="$HOME/.local/opt/locallmos/llama"
    MODELS_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/locallmos/models"
    SUDO=""
  fi
  _marker="$LLAMA_DIR/.locallmos-llamacpp"

  _tag="$(resolve_llamacpp_tag)"
  [ -n "$_tag" ] || { echo "could not resolve llama.cpp version" >&2; exit 1; }

  _forced=""
  if [ "$_backend" = "auto" ]; then
    _target="$(llamacpp_detect_backend)"
    _chain="$(llamacpp_backend_chain "$_target")"
  else
    _forced="yes"
    _target="$_backend"
    _chain="$_backend"
  fi
  echo "==> Provisioning llama.cpp: tag=$_tag mode=$_backend target=$_target ($OS-$ARCH)"

  # Idempotency: reuse an existing install iff the marker records the same backend
  # and tag. A missing marker (all legacy installs) reprovisions once.
  _existing="$(find "$LLAMA_DIR" -type f -name llama-server 2>/dev/null | head -n1)"
  if [ -n "$_existing" ] && [ -f "$_marker" ]; then
    _mbackend="$(sed -n 's/^backend=//p' "$_marker" 2>/dev/null | head -n1)"
    _mtag="$(sed -n 's/^tag=//p' "$_marker" 2>/dev/null | head -n1)"
    if [ "$_mbackend" = "$_target" ] && [ "$_mtag" = "$_tag" ]; then
      echo "==> llama-server already provisioned (backend=$_mbackend tag=$_mtag)"
      LLAMA_BIN="$_existing"
      LLAMA_BACKEND="$_mbackend"
      $SUDO mkdir -p "$MODELS_DIR"
      echo "==> llama.cpp models dir: $MODELS_DIR  (drop your .gguf files here)"
      return 0
    fi
  fi

  # Provision loop: stage each candidate into a fresh temp dir, smoke test it, and
  # commit the first one that passes. The old install is only deleted after a
  # replacement passes its smoke test.
  _committed=""
  for _b in $_chain; do
    echo "==> staging backend: $_b"
    _stage="$(mktemp -d)"
    if ! llamacpp_stage_prebuilt "$_b" "$_tag" "$_stage"; then
      rm -rf "$_stage"
      if [ -n "$_forced" ]; then
        echo "forced backend '$_b' could not be provisioned; drop --llamacpp-backend for auto fallback" >&2
        exit 1
      fi
      continue
    fi
    if ! llamacpp_smoke_test "$_stage"; then
      echo "   smoke test failed for '$_b'"
      rm -rf "$_stage"
      if [ -n "$_forced" ]; then
        echo "forced backend '$_b' failed its smoke test; drop --llamacpp-backend for auto fallback" >&2
        exit 1
      fi
      continue
    fi
    echo "==> installing backend '$_b' to $LLAMA_DIR"
    $SUDO rm -rf "$LLAMA_DIR"
    $SUDO mkdir -p "$LLAMA_DIR"
    $SUDO cp -a "$_stage/." "$LLAMA_DIR/"
    rm -rf "$_stage"
    LLAMA_BIN="$(find "$LLAMA_DIR" -type f -name llama-server 2>/dev/null | head -n1)"
    $SUDO chmod +x "$LLAMA_BIN"
    printf 'backend=%s\ntag=%s\n' "$_b" "$_tag" | $SUDO tee "$_marker" >/dev/null
    LLAMA_BACKEND="$_b"
    _committed="yes"
    break
  done

  if [ -z "$_committed" ]; then
    if [ -n "$_existing" ]; then
      echo "!! no backend in the chain could be provisioned; keeping the existing install ($_existing)" >&2
      LLAMA_BIN="$_existing"
      LLAMA_BACKEND="$(sed -n 's/^backend=//p' "$_marker" 2>/dev/null | head -n1)"
    else
      echo "could not provision any llama.cpp backend for $OS-$ARCH at $_tag" >&2
      exit 1
    fi
  fi

  $SUDO mkdir -p "$MODELS_DIR"
  echo "==> llama.cpp backend: ${LLAMA_BACKEND:-unknown}  bin: $LLAMA_BIN"
  echo "==> llama.cpp models dir: $MODELS_DIR  (drop your .gguf files here)"
}
