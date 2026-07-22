//! Lightweight GPU-vendor detection for a startup sanity check: warn when the
//! provisioned llama.cpp backend looks wrong for the hardware we can see (e.g. a
//! `cuda` build on a box with no NVIDIA GPU). Mirrors the installer's iGPU policy
//! (service/lib-llamacpp.sh) so the agent and installer agree. No new deps.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuVendor {
    Nvidia,
    Amd,
    /// A qualifying discrete card that is neither NVIDIA nor AMD (e.g. Intel Arc),
    /// or Apple Silicon (→ metal). Vulkan/metal territory.
    Other,
    /// No qualifying discrete GPU — CPU-only (or iGPU-only, which we treat as CPU).
    None,
}

/// Detect the dominant discrete GPU vendor using the same policy as the installer:
/// NVIDIA via `nvidia-smi -L`; else a qualifying AMD dGPU/APU; else any other
/// qualifying discrete card; else none. Apple Silicon reports `Other` (metal).
pub fn detect() -> GpuVendor {
    if cfg!(target_os = "macos") {
        return GpuVendor::Other; // Apple Silicon → metal
    }
    if nvidia_present() {
        return GpuVendor::Nvidia;
    }
    if qualifying_gpu("0x1002") {
        return GpuVendor::Amd;
    }
    if qualifying_gpu("0x8086") || qualifying_gpu("0x10de") {
        return GpuVendor::Other;
    }
    GpuVendor::None
}

fn nvidia_present() -> bool {
    std::process::Command::new("nvidia-smi")
        .arg("-L")
        .output()
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false)
}

/// Is a discrete-class card of `vendor` present? Qualifies on >= 4 GiB dedicated
/// VRAM (amdgpu `mem_info_vram_total`) or a whitelisted unified-memory APU device
/// id (Strix Halo `0x1586`) — the same heuristic the installer uses, so a small
/// iGPU doesn't count.
fn qualifying_gpu(vendor: &str) -> bool {
    const MIN_VRAM: u64 = 4 * 1024 * 1024 * 1024; // 4 GiB
    let Ok(cards) = std::fs::read_dir("/sys/class/drm") else {
        return false;
    };
    for card in cards.flatten() {
        let dev = card.path().join("device");
        match read_trim(&dev.join("vendor")) {
            Some(v) if v == vendor => {}
            _ => continue,
        }
        if let Some(vram) = read_trim(&dev.join("mem_info_vram_total"))
            .and_then(|s| s.parse::<u64>().ok())
        {
            if vram >= MIN_VRAM {
                return true;
            }
        }
        if read_trim(&dev.join("device")).as_deref() == Some("0x1586") {
            return true;
        }
    }
    false
}

fn read_trim(p: &Path) -> Option<String> {
    std::fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

/// Backends that make sense for a vendor, best first. `vulkan`/`cpu` are omitted
/// from the "specific" check by [`is_plausible`] because they run almost anywhere.
pub fn expected_backends(vendor: GpuVendor) -> &'static [&'static str] {
    match vendor {
        GpuVendor::Nvidia => &["cuda", "vulkan", "cpu"],
        GpuVendor::Amd => &["rocm", "vulkan", "cpu"],
        GpuVendor::Other => &["metal", "vulkan", "cpu"],
        GpuVendor::None => &["cpu"],
    }
}

/// Whether `backend` is a reasonable choice for `vendor`. `cpu` and `vulkan` run
/// on virtually anything, so they never count as a mismatch; the vendor-specific
/// backends (`cuda`/`rocm`/`metal`) must match the detected hardware.
fn is_plausible(backend: &str, vendor: GpuVendor) -> bool {
    backend == "cpu" || backend == "vulkan" || expected_backends(vendor).contains(&backend)
}

/// Warn (once, at startup) when the active llama.cpp backend doesn't match the
/// detected GPU vendor — e.g. a `cuda` build with no NVIDIA GPU visible. Never
/// fatal: the install was smoke-tested and already runs; this only flags a
/// surprising configuration and points at the fix.
pub fn warn_on_mismatch(backend: &str) {
    let vendor = detect();
    if is_plausible(backend, vendor) {
        return;
    }
    tracing::warn!(
        "llama.cpp backend '{backend}' does not match detected GPU vendor {vendor:?} \
         (expected one of {:?}); if inference misbehaves, reprovision with \
         LOCALLMOS_LLAMACPP_BACKEND=auto",
        expected_backends(vendor)
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expected_backends_lead_with_the_vendor_native_engine() {
        assert_eq!(expected_backends(GpuVendor::Nvidia)[0], "cuda");
        assert_eq!(expected_backends(GpuVendor::Amd)[0], "rocm");
        assert_eq!(expected_backends(GpuVendor::Other)[0], "metal");
        assert_eq!(expected_backends(GpuVendor::None), &["cpu"]);
    }

    #[test]
    fn cpu_and_vulkan_are_always_plausible() {
        for v in [GpuVendor::Nvidia, GpuVendor::Amd, GpuVendor::Other, GpuVendor::None] {
            assert!(is_plausible("cpu", v));
            assert!(is_plausible("vulkan", v));
        }
    }

    #[test]
    fn vendor_specific_backends_must_match() {
        assert!(is_plausible("cuda", GpuVendor::Nvidia));
        assert!(!is_plausible("cuda", GpuVendor::Amd));
        assert!(!is_plausible("cuda", GpuVendor::None));
        assert!(is_plausible("rocm", GpuVendor::Amd));
        assert!(!is_plausible("rocm", GpuVendor::Nvidia));
        assert!(is_plausible("metal", GpuVendor::Other));
        assert!(!is_plausible("metal", GpuVendor::Nvidia));
    }
}
