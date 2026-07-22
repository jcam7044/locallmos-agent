# Hardware-aware llama.cpp backend selection (Unsloth-style install flow)

## Context

The `locallmos-agent` installers provision llama.cpp for the `llamacpp` runtime, but backend selection is crude: on Linux, *any* GPU signal (`nvidia-smi`, `/dev/dri`, or `vulkaninfo`) picks the Vulkan prebuilt, else CPU; Windows (`install.ps1`) doesn't provision llama.cpp at all. The goal is to match Unsloth's install flow: detect OS + GPU vendor → install the best backend (CUDA / ROCm / Vulkan / CPU / Metal) with a graceful fallback chain, plus a lightweight hardware re-detection in the agent at startup.

**User decisions:** Linux NVIDIA → download a **self-hosted CUDA prebuilt** (LocalLMOS publishes Linux CUDA builds from its own CI, since upstream llama.cpp ships none — see [llamacpp-prebuilt-hosting-plan.md](llamacpp-prebuilt-hosting-plan.md)), falling back to the Vulkan prebuilt → CPU. Linux AMD → ROCm prebuilt first (when a ROCm runtime is detected) → Vulkan → CPU. Scope includes the sh installers, Windows `install.ps1` provisioning parity, and Rust agent startup detection. *(Superseded: CUDA was originally an on-device source build; it is now a plain prebuilt download on the same code path as the other backends.)*

**Verified facts:**
- Pinned tag `b10068` (and latest) ships: `ubuntu-rocm-7.2-x64`, `ubuntu-vulkan-{x64,arm64}`, `ubuntu-{x64,arm64}`, `win-cuda-{12.4,13.3}-x64` (+ companion `cudart-llama-bin-win-cuda-*.zip` runtime DLLs), `win-hip-radeon-x64`, `win-vulkan-x64`, `win-cpu-{x64,arm64}`, `macos-{arm64,x64}`. Upstream ships **no Linux CUDA prebuilt** — LocalLMOS now publishes its own (`ubuntu-cuda-{12.4,13.3}-x64`) from `.github/workflows/llamacpp-prebuilt.yml` into this repo's releases, so CUDA is a plain download like every other backend (was: on-device source build).
- `service/lib-llamacpp.sh` is fetched from `raw.githubusercontent.com` by already-shipped `install.sh` copies → it must stay self-contained and backward compatible (new globals need safe defaults; `provision_llamacpp` stays the sole entry point).
- Cloud `runtimes` table has a free-text `version` column, always `None` for llamacpp today → backend string can surface there with zero schema changes.
- This dev machine: NVIDIA + AMD GPUs, `nvidia-smi`, `cmake`, `dash` present; no `nvcc`/`vulkaninfo`/`rocminfo` — good for exercising fallback chains.

## Files

| File | Change |
|---|---|
| `service/lib-llamacpp.sh` | Detection + chain + staged provisioning (core) |
| `service/install.sh`, `service/install-service.sh` | `--llamacpp-backend` flag, backend env plumbing |
| `service/install.ps1` | New llama.cpp provisioning with detection |
| `src-tauri/src/hardware.rs` (new), `runtime/llama_server.rs`, `runtime/mod.rs`, `lib.rs`, `status.rs`, `worker.rs`, `supabase.rs` | Startup detection + backend surfacing |
| `SERVICE.md`, `README.md`, `service/agent.env.example` | Docs |

## 1. `service/lib-llamacpp.sh` (POSIX sh — must parse under dash)

New caller global `LLAMACPP_BACKEND` (default `auto`; `auto|cuda|rocm|vulkan|cpu|metal`); new output global `LLAMA_BACKEND` alongside `LLAMA_BIN`/`MODELS_DIR`.

New helpers (all logging to stderr; stdout is the return channel):
- `_llx_has_gpu_vendor <0xVVVV>` — scan `/sys/class/drm/card*/device/vendor` (guard globs with `[ -f ] || continue`).
- `_llx_cuda_variant` — echoes the hosted CUDA prebuilt variant for this rig from `nvidia-smi`'s reported CUDA/driver version: ≥ 13.0 → `13.3` (Blackwell-capable), else ≥ 12.4 → `12.4`, else empty (driver too old / unreadable → caller falls back to `vulkan`). Mirrors the Windows `Get-LlamaCppBackend` 12.4/13.3 split; no build toolchain is required anymore.
- `_llx_qualifying_gpu <vendor_id>` — **iGPU policy**: an integrated GPU must NOT steer backend choice (e.g. a small AMD iGPU next to NVIDIA dGPUs must not drag the machine toward Vulkan/ROCm). A drm card of the given vendor qualifies only if it is discrete-class — `mem_info_vram_total` ≥ 4 GiB (dedicated VRAM heuristic) — OR its PCI device ID is on a small unified-memory APU whitelist (initially Strix Halo `0x1586` / gfx1151; comment that the list is meant to grow). Apple Silicon is already handled by the macOS→metal branch; DGX Spark (NVIDIA Grace-Blackwell) needs no whitelist entry because `nvidia-smi` routes it down the NVIDIA branch.
- `llamacpp_detect_backend` — echoes one word:
  - macOS → `metal`
  - Linux: `nvidia-smi -L` succeeds → `cuda` if `_llx_cuda_variant` is non-empty else `vulkan` (log when the driver is too old for any hosted CUDA build); elif `_llx_qualifying_gpu 0x1002` (AMD dGPU or whitelisted APU) → `rocm` if x86_64 + `/dev/kfd` + (`rocminfo`/`rocm-smi`/`/opt/rocm`) else `vulkan`; elif any other qualifying non-AMD, non-NVIDIA drm card (e.g. Intel Arc with ≥ 4 GiB VRAM) → `vulkan`; else `cpu` — an unqualified iGPU-only machine gets the CPU build, and mere `/dev/dri`/`vulkaninfo` presence no longer counts as a GPU signal. NVIDIA checked before AMD (mixed rigs prefer CUDA; the agent's existing `pick_devices` handles device choice at run time).
- `llamacpp_backend_chain` — `cuda→"cuda vulkan cpu"`, `rocm→"rocm vulkan cpu"`, `vulkan→"vulkan cpu"`, `cpu→"cpu"`, `metal→"metal"` (space-separated string; POSIX has no arrays).
- `llamacpp_asset_for <backend> <tag>` — maps to asset filename (`rocm` = `ubuntu-rocm-7.2-x64`, x86_64 only — comment that "7.2" must be revisited when bumping the pinned tag); `cuda` = `ubuntu-cuda-<variant>-x64` where `<variant>` comes from `_llx_cuda_variant` (x86_64 only; served from the self-hosted `LLAMACPP_REPO`, not upstream).
- `_llx_asset_repo <backend>` — returns the GitHub `owner/repo` to download from. **This is the key mixed-source hook:** CUDA lives in the self-hosted repo (`$LLAMACPP_CUDA_REPO`, default `jcam7044/locallmos-agent`), every other backend in upstream (`$LLAMACPP_REPO`, default `ggml-org/llama.cpp`). If a future `LLAMACPP_VERSION` release mirrors *all* Linux backends into the self-hosted repo (the hosting workflow's `mirror_backends` option), point `LLAMACPP_REPO` at it too and this helper collapses to one source. Keep both overrides even when they're equal.
- `llamacpp_stage_prebuilt <backend> <tag> <stagedir>` — resolve base via `_llx_asset_repo`, download `https://github.com/<repo>/releases/download/$tag/$asset` + extract into a staging dir; nonzero on failure. **CUDA now flows through this path like every other backend** — the extracted tree carries `llama-server` plus its bundled CUDA runtime libs ($ORIGIN rpath), so the existing whole-tree copy just works. No source build, no toolchain, no 10–30 min wait. *(Removed: the former `llamacpp_stage_cuda_build` on-device compile helper.)*
- `llamacpp_smoke_test <dir>` — find llama-server, `chmod +x`, run `--version`; return its status.

Restructured `provision_llamacpp`:
- Resolve tag; `target` = forced backend or `llamacpp_detect_backend`; `chain` = single entry (forced) or `llamacpp_backend_chain`.
- Marker file `$LLAMA_DIR/.locallmos-llamacpp` (`backend=…` / `tag=…`).
- **Idempotency rule:** reuse iff llama-server exists AND marker backend == target AND marker tag == tag. Everything else — including missing marker (all legacy installs) — reprovisions once. *(No CUDA-specific reuse exception is needed now that CUDA is a download, not a fragile source build tied to a toolchain that could vanish.)*
- **Provision loop:** for each backend in chain → stage a prebuilt into a fresh `mktemp -d` → smoke test in staging → on pass, commit: `$SUDO rm -rf "$LLAMA_DIR"`, copy staging in, write marker, set `LLAMA_BACKEND`/`LLAMA_BIN`, break. Old install is only deleted after a replacement passes its smoke test.
- **Forced backend hard-fails** on any failure (no silent degradation — forcing is for debugging/known hardware; error message says to drop the flag for auto fallback). Auto mode: if the whole chain fails but an old install exists, keep it and warn; else exit 1.

POSIX pitfalls: no arrays/`local`/`pipefail`/portable `sed -i`; helpers that may fail must be called in `if`/`||` context (`set -e` in callers); `_llx_` prefix for internals.

## 2. `install.sh` / `install-service.sh` plumbing

- Default `LLAMACPP_BACKEND="${LOCALLMOS_LLAMACPP_BACKEND:-auto}"`; new `--llamacpp-backend` flag + validation case, next to the existing `--runtime` handling (install.sh ~line 55, install-service.sh ~line 22).
- New `LLAMACPP_CUDA_REPO="${LOCALLMOS_LLAMACPP_CUDA_REPO:-jcam7044/locallmos-agent}"` beside the existing `LLAMACPP_REPO` default in both installers (install.sh ~line 29, install-service.sh ~line 16) — the source of the self-hosted CUDA prebuilts, consumed by `_llx_asset_repo`. Leave `LLAMACPP_REPO` defaulting to `ggml-org/llama.cpp` (upstream still serves Vulkan/ROCm/CPU) until the self-hosted releases mirror those too.
- install.sh line 313: append `LOCALLMOS_LLAMACPP_BACKEND=$LLAMA_BACKEND` to `RUNTIME_ENV` (flows into the `.desktop` Exec line and nohup launch unchanged).
- Service-mode `agent.env` blocks (install.sh 342–350, install-service.sh 88–94): the current `grep -q '^LOCALLMOS_RUNTIME='` guard leaves stale BIN/backend after reprovision — replace with a portable rewrite (`grep -v` the four `LOCALLMOS_RUNTIME/LLAMACPP_*` keys into `agent.env.new`, append fresh lines, `mv`, `chmod 0600`).

## 3. `install.ps1` — Windows parity

- New params (env-var defaulted like the rest): `-Runtime` (`ollama`|`llamacpp`), `-LlamaCppVersion` (`b10068`), `-LlamaCppRepo`, `-LlamaCppBackend` (`auto|cuda|hip|vulkan|cpu`); validate after the elevation check.
- Detection (`Get-LlamaCppBackend`): `nvidia-smi` CUDA version ≥ 12.4 → `cuda` (≥ 13.0 picks the 13.3 zip pair, else 12.4 pair; both need the companion `cudart-*.zip` extracted alongside); AMD → `hip` only for a *qualifying* controller: `Win32_VideoController` whose name does NOT match the iGPU patterns the Rust `pick_devices` already filters (`Radeon(TM)? Graphics$`, `Intel.*(UHD|Iris|HD Graphics)`) or DOES match the unified-memory whitelist (`Radeon 8050S|8060S` — Strix Halo), plus `amdhip64*.dll` in System32 (HIP build dynamically loads the driver's HIP runtime — detection is best-effort, smoke-test fallback to vulkan covers it); any other qualifying non-iGPU controller → `vulkan`; else (iGPU-only or none) → `cpu`. Name-based qualification is used instead of `AdapterRAM` (uint32, caps at 4 GB — unreliable).
- `Install-LlamaCpp`: install to `$env:ProgramFiles\LocalLMOS\llama`; models dir `$env:APPDATA\locallmos\models` (desktop) / `$env:ProgramData\locallmos\models` (service). Same marker/idempotency rule and staged smoke-test chain walk as §1 (`Invoke-WebRequest` in try/catch, `Expand-Archive` all assets into one staging dir, run `llama-server.exe --version`); forced backend `throw`s.
- Env wiring for `llamacpp`: service mode → machine-scoped `[Environment]::SetEnvironmentVariable` for `LOCALLMOS_RUNTIME`, `LOCALLMOS_LLAMACPP_BIN/_MODELS_DIR/_BACKEND` (next to existing machine vars, lines 117–119); desktop mode → process env before `Start-Process`, with the Rust `default_bin()` Windows roots (§4) covering later launches.
- Guard the bottom Ollama warning with `if ($Runtime -eq "ollama")`; print a llama.cpp summary otherwise.

## 4. Rust agent (startup detection + surfacing)

- `runtime/llama_server.rs`: `LlamaServerAdapter.backend: Option<String>` from `LOCALLMOS_LLAMACPP_BACKEND`, falling back to reading the `.locallmos-llamacpp` marker beside the resolved binary; fill new `RuntimeSnapshot.backend` in `snapshot()`; add Windows roots (`%ProgramFiles%/%ProgramData%\LocalLMOS\llama`) to `default_bin()` (~line 1070).
- New `src-tauri/src/hardware.rs` (~80 lines, no new deps): `GpuVendor {Nvidia, Amd, Other, None}` + `detect()` applying the same iGPU policy as the installers (Linux: `nvidia-smi -L`; else AMD only when a drm `0x1002` card has ≥ 4 GiB `mem_info_vram_total` or a whitelisted device ID (`0x1586` Strix Halo); else Other only for a qualifying discrete card; Windows: `nvidia-smi` / HIP DLLs + iGPU name filter; macOS: n/a), `expected_backends(vendor)` map, `warn_on_mismatch(backend)` (`tracing::warn!`). Unit-test the mapping.
- `lib.rs`: call `warn_on_mismatch` once at startup when runtime is llamacpp and backend is known (the Unsloth `detect_hardware()` analog); include `"backend"` in the local snapshot JSON.
- `status.rs` + `worker.rs::telemetry_tick`: add `runtime_backend: Option<String>` to `AgentStatus` (tray).
- `supabase.rs::upsert_runtime`: send `"version": snap.version.clone().or_else(|| snap.backend.clone())` — backend lands in the existing `runtimes.version` text column, **zero schema changes**. (Optional follow-up in the main locallmOS repo: dedicated `runtimes.backend` column.)

## 5. Docs

- `SERVICE.md` (llamacpp section, ~lines 63–90): detection matrix, self-hosted Linux CUDA prebuilts + driver-version → `12.4`/`13.3` variant selection (no toolchain, no build wait), fallback chains, `--llamacpp-backend` / `LOCALLMOS_LLAMACPP_BACKEND` (forced = hard-fail), reprovision rule, Windows `-Runtime llamacpp`.
- `README.md` if it mentions `--runtime`; `service/agent.env.example`: commented `LOCALLMOS_LLAMACPP_BACKEND` line; `install.ps1` header comment.

## Future: llama.cpp click-to-update (design constraint, not in scope)

A later feature will check for llama.cpp updates and offer one-click updating. This plan is deliberately update-friendly: the marker file (`backend=`/`tag=`) makes the update check a tag comparison; staged provisioning makes updates atomic (a failed download never destroys the working install); and `provision_llamacpp` is re-runnable with a new `LLAMACPP_VERSION`, so the updater reuses this exact code path. The former CUDA cost — a 10–30 min on-device rebuild per update, plus a toolchain that had to stay installed — is now gone: CUDA is a plain asset download from the self-hosted `LLAMACPP_REPO` (see [llamacpp-prebuilt-hosting-plan.md](llamacpp-prebuilt-hosting-plan.md)), identical to every other backend. Do not remove the `LLAMACPP_REPO` indirection — it is what points CUDA rigs at the self-hosted builds.

## Sequencing

1. lib-llamacpp.sh (everything depends on `LLAMA_BACKEND`) → 2. sh installer plumbing → 3. install.ps1 → 4. Rust (parallel-safe with 2–3) → 5. docs.

## Verification

- **Lint/parse:** `dash -n service/lib-llamacpp.sh service/install.sh`; `bash -n service/install-service.sh`; `shellcheck` if available; `pwsh -NoProfile` syntax check for install.ps1 if pwsh exists.
- **Detection branches (this machine — NVIDIA Blackwell dGPUs + small AMD iGPU):** `dash -c 'OS=linux ARCH=x86_64 MODE=desktop; . service/lib-llamacpp.sh; llamacpp_detect_backend'` → expect `cuda` (nvidia-smi present, reports CUDA ≥ 13.0 → `_llx_cuda_variant`=`13.3`). Stub `nvidia-smi` to report CUDA 12.x → still `cuda`, variant `12.4`; stub it to report an ancient/unparseable version (`_llx_cuda_variant` empty) → `vulkan`. Stub failing `nvidia-smi` → the AMD iGPU must be **rejected** by the qualifying-GPU check (VRAM < 4 GiB, device ID not whitelisted) → expect `cpu`, which directly verifies the iGPU policy; temporarily whitelist the iGPU's device ID to watch the AMD→`vulkan` branch fire.
- **Provisioning sandbox (no root):** `HOME=/tmp/llx-test MODE=desktop OS=linux ARCH=x86_64 LLAMACPP_VERSION=b10068 LLAMACPP_BACKEND=cpu dash -c '. service/lib-llamacpp.sh; provision_llamacpp'` → installs + marker; re-run → reuse; re-run with `LLAMACPP_BACKEND=vulkan` → reprovision, and hard-fail if vulkan smoke test fails (verifies forced semantics). Auto mode exercises the real vulkan→cpu smoke fallback. Bogus `LLAMACPP_VERSION` → clean error; dummy `llama-server` without marker → legacy reprovision.
- **CUDA path:** with `LOCALLMOS_LLAMACPP_REPO` pointed at the self-hosted builds and a recent NVIDIA driver: auto mode → downloads `ubuntu-cuda-13.3-x64` (Blackwell) → marker `backend=cuda`; a rig reporting CUDA < 13.0 → `ubuntu-cuda-12.4-x64`. Stub `nvidia-smi` reporting an ancient driver (`_llx_cuda_variant` empty) → falls back to `vulkan`; a 404 on the CUDA asset (tag not yet built) → download fails and auto mode falls back to `vulkan`.
- **Rust:** `cargo check && cargo test` in src-tauri. `LOCALLMOS_RUNTIME=llamacpp LOCALLMOS_LLAMACPP_BACKEND=metal` on this Linux box → mismatch warning; `=cuda` → none. Backend visible in tray snapshot and in `runtimes.version` after a telemetry tick.
- **Windows:** needs a VM/box — NVIDIA (cuda + cudart co-extracted), forced `vulkan`/`cpu`, AMD hip→vulkan smoke fallback.
