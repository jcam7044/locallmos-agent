# Self-hosted llama.cpp Linux prebuilts (GitHub Actions release CI)

## Context

The backend-selection plan ([llamacpp-backend-selection-plan.md](llamacpp-backend-selection-plan.md))
compiles llama.cpp from source with CUDA **on the target machine** because
upstream `ggml-org/llama.cpp` ships no Linux CUDA prebuilt. That costs every
CUDA rig a 10–30 min build per install/update and depends on a full toolchain
(`nvcc` + `cmake` + C++ compiler) staying installed.

This plan replaces that with a GitHub Actions workflow that **builds Linux
llama.cpp binaries once, in CI, and hosts them as release assets** — so CUDA
(and, optionally, the other Linux backends) become plain downloads on the same
`releases/download/$tag/$asset` code path the installer already uses. It is the
"designated future fix" the backend plan calls out at line 81; nothing in that
plan blocks it, and the existing `LLAMACPP_REPO` indirection is the hook.

**What upstream already ships (Linux, tag `b10068`):** `ubuntu-rocm-7.2-x64`,
`ubuntu-vulkan-{x64,arm64}`, `ubuntu-{x64,arm64}` (CPU). **The only gap is CUDA.**
So the required deliverable is **Linux CUDA**; mirroring ROCm/Vulkan/CPU is an
optional reliability upgrade (single pinned source, immune to upstream deleting
old release assets), not a functional necessity.

**Verified facts:**
- `LLAMACPP_REPO` defaults to `ggml-org/llama.cpp` in both `install.sh:29` and
  `install-service.sh:16`, overridable via `LOCALLMOS_LLAMACPP_REPO`. The
  download URL is built as `https://github.com/$LLAMACPP_REPO/releases/download/$tag/$asset`
  (`lib-llamacpp.sh:86`). Point that at a builds repo and the existing download
  path works unchanged.
- Asset names follow the upstream convention `llama-<tag>-bin-<platform>.tar.gz`
  and each tarball carries `llama-server` plus its co-located shared libs
  ($ORIGIN-relative rpath); the installer copies the extracted tree whole.
- Upstream's **Windows** CUDA assets already split by toolkit version
  (`win-cuda-12.4-x64`, `win-cuda-13.3-x64`) — this plan mirrors that naming for
  Linux (`ubuntu-cuda-12.4-x64`, `ubuntu-cuda-13.3-x64`) so detection logic is
  symmetric with the `install.ps1` path in the backend plan (§3).
- Existing release CI (`.github/workflows/release.yml`) establishes the house
  patterns to reuse: `strategy.matrix`, `softprops/action-gh-release@v2`,
  sha256 sidecars, and (optionally) minisign signing.
- Building CUDA does **not** require a GPU on the runner — only `nvcc`. A GPU-less
  GitHub-hosted runner compiles fine; the CI smoke test is limited to
  `llama-server --version` (does not initialize CUDA).

## Decisions to confirm

1. **Host repo:** dedicated `jcam7044/locallmos-llamacpp-builds` (recommended —
   keeps llama.cpp releases out of the agent's `v*` release list) **vs** the
   existing `jcam7044/locallmos-agent` repo with upstream-style tags like
   `b10068` (lower effort; `release.yml` only triggers on `v*` so no collision,
   but clutters the agent release list). Either way the installer just needs
   `LLAMACPP_REPO` pointed at it.
2. **Scope:** CUDA-only (Phase 1) **vs** mirror all Linux backends into one
   release (recommended for a single pinned source). Mirroring adds a
   re-upload step, not a build.
3. **CUDA variants:** two — `cuda-12.4` (Turing→Hopper, older drivers) and
   `cuda-13.3` (adds Blackwell sm_120, needs a newer driver) — matching
   upstream Windows and the backend plan's driver→variant selection.

## Files

| File | Change |
|---|---|
| `.github/workflows/llamacpp-prebuilt.yml` (new) | Matrix build + publish of Linux backends for a given upstream tag |
| `docs/llamacpp-backend-selection-plan.md` | Retarget CUDA from source build to prebuilt (see §5 below) |
| `service/install.sh`, `service/install-service.sh` | `LLAMACPP_REPO` default (or per-asset base) if hosting in a dedicated repo |
| `SERVICE.md`, `README.md` | Document the hosted-CUDA path; drop on-device build prose |

## 1. The workflow — `.github/workflows/llamacpp-prebuilt.yml`

**Trigger:** push a git tag named after the llama.cpp build (`on: push: tags:
["b[0-9]*"]`, e.g. `b10068`) — same tag-push flow as the `v*` Release workflow
and doable from GitHub Desktop. The pushed tag name **is** the upstream
`ggml-org/llama.cpp` tag: the workflow checks out llama.cpp at it and **publishes
its own release named identically**, so `releases/download/$tag/$asset` resolves
with zero installer changes. `b*` and `v*` are disjoint, so this never collides
with the agent Release workflow. Caveat: a tag-push run uses the workflow file
**as it exists at the tagged commit**, so the tagged commit must already contain
`llamacpp-prebuilt.yml`.

`workflow_dispatch` is kept as a manual fallback with inputs:
- `tag` (required) — build a specific llama.cpp tag without pushing a git tag
  (re-run/backfill).
- `mirror_backends` (bool, default `false`) — also re-upload upstream's
  `ubuntu-rocm-7.2-x64`, `ubuntu-vulkan-{x64,arm64}`, `ubuntu-{x64,arm64}` assets
  into this release so the whole fallback chain comes from one pinned source.
  Only available on manual runs (tag pushes never mirror).

**Permissions:** `contents: write` (create release + upload assets), same as
`release.yml`.

**Build matrix (the CUDA jobs — the actual deliverable):**

| variant | container | CUDA arch list (`CMAKE_CUDA_ARCHITECTURES`) | asset |
|---|---|---|---|
| cuda-12.4 | `nvidia/cuda:12.4.1-devel-ubuntu22.04` | `75;80;86;89;90` (Turing→Hopper) | `llama-<tag>-bin-ubuntu-cuda-12.4-x64.tar.gz` |
| cuda-13.3 | `nvidia/cuda:13.3.0-devel-ubuntu22.04` | `75;80;86;89;90;100;120` (adds Blackwell) | `llama-<tag>-bin-ubuntu-cuda-13.3-x64.tar.gz` |

- Run on `ubuntu-22.04` (glibc 2.35 baseline, matches upstream `ubuntu-*` assets)
  with `container:` set to the CUDA devel image so `nvcc` is present.
- Install build deps in-container: `cmake`, `build-essential`, `git`, `libcurl4-openssl-dev`
  (or build with `-DLLAMA_CURL=OFF` to drop the runtime curl dep — recommended,
  matches the backend plan's source-build flags).
- Checkout llama.cpp at `${{ inputs.tag }}` (`actions/checkout` with
  `repository: ggml-org/llama.cpp`, `ref: <tag>`).
- Configure/build (mirrors the backend plan's `llamacpp_stage_cuda_build` flags,
  minus `-native`):
  ```
  cmake -S . -B build -DCMAKE_BUILD_TYPE=Release -DGGML_CUDA=ON \
    -DCMAKE_CUDA_ARCHITECTURES="<list>" \
    -DBUILD_SHARED_LIBS=OFF -DLLAMA_CURL=OFF -DLLAMA_BUILD_TESTS=OFF
  cmake --build build --target llama-server -j"$(nproc)"
  ```
  `CMAKE_CUDA_ARCHITECTURES` is an explicit list (we do **not** build on the
  target, so `native` is out). `BUILD_SHARED_LIBS=OFF` sidesteps build-tree
  rpaths for llama.cpp's own libs.
- **CUDA runtime bundling:** the binary still needs `libcudart`, `libcublas`,
  `libcublasLt` at run time. Decide one of:
  - **(a) Bundle (recommended)** — copy the three redistributable `.so` files from
    the CUDA image into the staging dir next to `llama-server` and set an
    `$ORIGIN` rpath (`patchelf --set-rpath '$ORIGIN'`), mirroring how the
    installer keeps co-located libs. Self-contained; only needs a compatible
    NVIDIA **driver** on the target. NVIDIA's CUDA redistributable license permits
    shipping these libs — note it in the release body.
  - **(b) Static** — link the static CUDA runtime; cuBLAS static inflates the
    binary substantially. Simpler tarball, larger download.
- Stage: gather `llama-server` + bundled libs into
  `dist/llama-<tag>-bin-ubuntu-cuda-<ver>-x64/`, then `tar -czf` the asset.
- **CI smoke test:** `./llama-server --version` (no GPU on the runner — this only
  confirms the binary links and loads its libs; it does not init CUDA).

**Optional mirror job** (`if: inputs.mirror_backends`): download the four upstream
Linux assets for `$tag` from `ggml-org/llama.cpp` and re-stage them unchanged for
upload into this release.

**Publish job** (reuse `release.yml` patterns):
- `sha256sum` sidecar per asset (`<asset>.sha256`).
- Optional but recommended: minisign each asset with `MINISIGN_SECRET_KEY`
  (same secret as `release.yml`) — but note verifying it requires adding a
  minisign check to `lib-llamacpp.sh` (extra scope, §5). Phase 1 can ship sha256
  only.
- `softprops/action-gh-release@v2` with `tag_name: ${{ inputs.tag }}`,
  `files: dist/*`. If hosting in a **dedicated** repo, add `repository:` +
  a PAT/`GITHUB_TOKEN` scoped to that repo.

## 2. CUDA arch / driver compatibility notes

- CUDA 13 dropped Maxwell/Pascal/Volta (sm_50/60/70); the 12.4 variant is the
  floor for those and for older drivers. Detection (backend plan §1/§3) picks the
  **highest variant the installed driver supports**, preferring 13.3 on Blackwell —
  exactly the logic the Windows path already encodes (`nvidia-smi` CUDA version
  ≥ 13.0 → 13.3 else 12.4).
- Blackwell (sm_120, e.g. this dev machine's dGPUs) **requires the 13.3 build**;
  the 12.4 toolkit cannot emit sm_120. This is the concrete reason both variants
  exist rather than one.
- Bundled-runtime forward compat: the target's NVIDIA **driver** must be ≥ the
  bundled CUDA runtime's minimum (roughly ≥ 550 for 12.4, ≥ 580 for 13.3). If the
  driver is too old, the CUDA build's smoke test fails on the target and the
  installer's fallback chain (`cuda → vulkan → cpu`) covers it — no regression vs
  today.

## 3. Hosting / repo wiring

- **Dedicated repo (recommended):** create `jcam7044/locallmos-llamacpp-builds`;
  set `LLAMACPP_REPO` default to it in `install.sh` / `install-service.sh` (or add
  a narrower `LLAMACPP_CUDA_REPO` so only CUDA comes from the mirror while
  ROCm/Vulkan/CPU still hit upstream — but that reintroduces mixed sources, which
  the mirror job exists to avoid). Publishing needs a PAT with `contents:write` on
  the builds repo stored as a secret in whichever repo runs the workflow.
- **Same repo (lower effort):** publish under upstream-style tag `b10068` in
  `jcam7044/locallmos-agent`; `LLAMACPP_REPO` default becomes
  `jcam7044/locallmos-agent`. `release.yml` only triggers on `v*`, so a `b10068`
  tag won't fire it.

## 4. Release/update lifecycle

- **Bumping the pin:** to move from `b10068` to a newer llama.cpp, run the
  workflow with the new `tag`; it produces a matching release; then bump
  `LLAMACPP_VERSION` defaults in the installers. The marker-file idempotency in
  the backend plan makes the client update a tag comparison.
- **Reproducibility:** the workflow is pure inputs → assets (no local state); a
  given `tag` always builds the same sources. Record the CUDA toolkit versions in
  the release body.
- This directly enables the backend plan's "click-to-update": CUDA rigs update by
  downloading a new asset instead of a 10–30 min on-device rebuild.

## 5. Required edits to the backend-selection plan (do after this lands)

Once assets exist, the on-device CUDA compile is removed from
[llamacpp-backend-selection-plan.md](llamacpp-backend-selection-plan.md):

- **Delete** `llamacpp_stage_cuda_build` and its 10–30 min notice / source-tarball
  download / static-lib rationale (§1, line 39–40).
- **Repurpose** `_llx_cuda_build_ready`: CUDA no longer needs `nvcc`/`cmake`/
  compiler. Detection picks `cuda` whenever `nvidia-smi -L` succeeds, then maps the
  reported driver/CUDA version → `12.4` vs `13.3` variant (mirror the `install.ps1`
  `Get-LlamaCppBackend` logic). The "missing prereqs → fall back to vulkan" branch
  becomes "unsupported/too-old driver → vulkan".
- **`llamacpp_asset_for cuda <tag>`** returns the hosted asset name (was empty for
  source build) — needs the variant, so pass it a resolved `cuda_variant`
  (`12.4`/`13.3`) via arg or global.
- **CUDA now flows through `llamacpp_stage_prebuilt`** like every other backend;
  the provision loop, marker, smoke-test, and forced-backend semantics are
  unchanged. The special "keep a working source build when nvcc vanished" reuse
  exception (§1 line 46) is no longer needed.
- **`LLAMACPP_REPO`** default (or CUDA base) updated per §3; keep the indirection.
- **Docs** (`SERVICE.md`, backend plan §5): drop CUDA source-build prereqs/build
  time; describe hosted CUDA + driver-version → variant selection.
- **(Optional)** repoint Windows CUDA download at the mirror too for a single
  source; not required since upstream already ships Windows CUDA.
- **(Optional, if signing)** add minisign verification to `lib-llamacpp.sh`
  downloads.

Net effect on the backend plan: §1 shrinks (one fewer stage function, simpler
detection), the toolchain dependency disappears, and CUDA installs/updates become
as fast and atomic as the prebuilt backends.

## 6. Verification

- **Workflow (dry run):** `act` or a scratch branch — trigger with `tag=b10068`,
  `mirror_backends=false`; confirm both CUDA jobs compile in-container and the
  `--version` smoke test passes; inspect the tarball has `llama-server` + bundled
  `libcudart/libcublas/libcublasLt` (`ldd llama-server` resolves via `$ORIGIN`).
- **Asset naming:** published names exactly match what a patched
  `llamacpp_asset_for` will request (`llama-b10068-bin-ubuntu-cuda-13.3-x64.tar.gz`).
- **End-to-end on this dev machine (Blackwell dGPU):** point
  `LOCALLMOS_LLAMACPP_REPO` at the builds repo, run the (patched) installer with
  auto detection → downloads `cuda-13.3`, smoke test runs CUDA at chat time.
  Force `--llamacpp-backend cuda` on a 12.4-only-driver box → downloads `cuda-12.4`.
- **Fallback intact:** on a box with too-old a driver, the CUDA smoke test fails →
  installer proceeds to `vulkan` → `cpu` with no regression.
- **Checksum:** `sha256sum -c` the sidecar against the downloaded asset.
```
