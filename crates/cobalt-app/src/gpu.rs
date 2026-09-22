//! GPU adapter selection and a note of what we ended up rendering with.
//!
//! wgpu's default pick is fine on a desktop, but on a VM or an RDP session without a GPU the
//! only adapter is Microsoft's software rasterizer (WARP). We still use it — there is nothing
//! else — but we record the fact so the UI can spend less per frame, and `COBALT_ADAPTER=<name
//! substring>` forces a specific adapter (e.g. `basic render` to test the software path locally).

use eframe::egui_wgpu::wgpu;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

static SOFTWARE: AtomicBool = AtomicBool::new(false);
static ADAPTER: OnceLock<String> = OnceLock::new();
/// Set when we fell back to WARP without a Mesa DLL to offer instead: the app shows advice once.
static WARP_WITHOUT_MESA: AtomicBool = AtomicBool::new(false);

/// Name of the Mesa llvmpipe OpenGL library that turns the glow renderer into a fast software
/// rasterizer when placed next to the executable (Windows).
pub const MESA_DLL: &str = "opengl32.dll";

/// Decide the renderer before the window exists.
///
/// `setting` is `advanced.renderer`: `auto` (default; legacy `wgpu` means the same), `wgpu-only`
/// (never fall back), `opengl` (always glow). `COBALT_RENDERER=wgpu|glow|opengl` overrides.
///
/// Why: on a VM or RDP session without a GPU the only wgpu adapter is Microsoft's WARP, and
/// WARP's D3D12 path rasterizes an egui frame at ~4 fps at 1600×900 (measured). Mesa's llvmpipe
/// through OpenGL does the same frame at 200 fps on the same machine. So when there is no GPU and
/// a Mesa `opengl32.dll` sits next to the exe, use OpenGL.
pub fn choose_renderer(setting: &str) -> eframe::Renderer {
    let forced = std::env::var("COBALT_RENDERER").ok().map(|v| v.to_ascii_lowercase());
    let choice = forced.as_deref().unwrap_or(setting).to_ascii_lowercase();
    match choice.as_str() {
        "glow" | "opengl" | "software" => {
            tracing::info!("renderer: OpenGL (glow) by request");
            return eframe::Renderer::Glow;
        }
        "wgpu-only" | "gpu" => {
            tracing::info!("renderer: wgpu by request (no software fallback)");
            return eframe::Renderer::Wgpu;
        }
        _ => {}
    }
    if !cfg!(target_os = "windows") {
        return eframe::Renderer::Wgpu;
    }
    if has_gpu_adapter() {
        return eframe::Renderer::Wgpu;
    }
    if mesa_dll_path().is_some() {
        tracing::warn!("no GPU adapter: rendering in software with Mesa llvmpipe (OpenGL) — much faster than WARP");
        eframe::Renderer::Glow
    } else {
        WARP_WITHOUT_MESA.store(true, Ordering::Relaxed);
        tracing::warn!("no GPU adapter and no {MESA_DLL} next to the executable: falling back to WARP (slow). See docs/alpha_notes.md → Running without a GPU.");
        eframe::Renderer::Wgpu
    }
}

/// True when WARP is in use although a Mesa DLL would have been faster — surface advice once.
pub fn warp_without_mesa() -> bool {
    WARP_WITHOUT_MESA.load(Ordering::Relaxed)
}

/// The Mesa DLL next to the executable, if present.
pub fn mesa_dll_path() -> Option<std::path::PathBuf> {
    let p = std::env::current_exe().ok()?.parent()?.join(MESA_DLL);
    p.is_file().then_some(p)
}

/// Is there any adapter that is not a CPU rasterizer? (DX12 on Windows; cheap, no surface.)
fn has_gpu_adapter() -> bool {
    if std::env::var_os("COBALT_ASSUME_NO_GPU").is_some() {
        tracing::warn!("COBALT_ASSUME_NO_GPU set: pretending there is no GPU adapter");
        return false;
    }
    let backends = if cfg!(target_os = "windows") { wgpu::Backends::DX12 } else { wgpu::Backends::PRIMARY };
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor { backends, ..wgpu::InstanceDescriptor::new_without_display_handle_from_env() });
    let adapters = pollster::block_on(instance.enumerate_adapters(backends));
    let mut gpu = false;
    for a in &adapters {
        let info = a.get_info();
        tracing::debug!(name = %info.name, device_type = ?info.device_type, backend = ?info.backend, "adapter");
        if info.device_type != wgpu::DeviceType::Cpu {
            gpu = true;
        }
    }
    if adapters.is_empty() {
        tracing::warn!("wgpu found no adapters at all");
    }
    gpu
}

/// True when frames are rasterized on the CPU (no usable GPU): draw less, animate less.
pub fn is_software() -> bool {
    SOFTWARE.load(Ordering::Relaxed)
}

/// Record the OpenGL renderer when running through glow (software when it is llvmpipe/softpipe).
pub fn note_gl(renderer: &str, version: &str) {
    let software = renderer.to_ascii_lowercase().contains("llvmpipe") || renderer.to_ascii_lowercase().contains("softpipe") || renderer.contains("GDI Generic");
    SOFTWARE.store(software, Ordering::Relaxed);
    let _ = ADAPTER.set(format!("{renderer} (OpenGL {version})"));
    tracing::info!(renderer, version, software, "OpenGL renderer");
}

/// "<name> (<backend>, <device type>)" of the adapter in use, once known.
pub fn adapter_label() -> Option<&'static str> {
    ADAPTER.get().map(|s| s.as_str())
}

fn rank(info: &wgpu::AdapterInfo) -> u8 {
    use wgpu::DeviceType::*;
    match info.device_type {
        DiscreteGpu => 0,
        IntegratedGpu => 1,
        VirtualGpu => 2,
        Other => 3,
        Cpu => 4,
    }
}

/// The selector handed to eframe: honour `COBALT_ADAPTER`, else the best surface-compatible
/// adapter by device type (discrete > integrated > virtual > other > CPU).
pub fn selector() -> eframe::egui_wgpu::NativeAdapterSelectorMethod {
    Arc::new(|adapters: &[wgpu::Adapter], surface: Option<&wgpu::Surface<'_>>| -> Result<wgpu::Adapter, String> {
        let compatible = |a: &wgpu::Adapter| surface.map(|s| a.is_surface_supported(s)).unwrap_or(true);
        let mut list: Vec<&wgpu::Adapter> = adapters.iter().filter(|a| compatible(a)).collect();
        if list.is_empty() {
            list = adapters.iter().collect();
        }
        if list.is_empty() {
            return Err("no graphics adapter found".into());
        }
        let forced = std::env::var("COBALT_ADAPTER").ok().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty());
        let chosen = match &forced {
            Some(want) => match list.iter().find(|a| a.get_info().name.to_lowercase().contains(want.as_str())) {
                Some(a) => *a,
                None => {
                    tracing::warn!(want, "COBALT_ADAPTER matched no adapter; using the default pick");
                    list.sort_by_key(|a| rank(&a.get_info()));
                    list[0]
                }
            },
            None => {
                list.sort_by_key(|a| rank(&a.get_info()));
                list[0]
            }
        };
        let info = chosen.get_info();
        let label = format!("{} ({:?}, {:?})", info.name, info.backend, info.device_type);
        let software = info.device_type == wgpu::DeviceType::Cpu;
        SOFTWARE.store(software, Ordering::Relaxed);
        let _ = ADAPTER.set(label.clone());
        if software {
            tracing::warn!(adapter = %label, "no GPU available: rendering in software (WARP). Expect slower drawing; animations are reduced.");
        } else {
            tracing::info!(adapter = %label, "graphics adapter");
        }
        Ok(chosen.clone())
    })
}
