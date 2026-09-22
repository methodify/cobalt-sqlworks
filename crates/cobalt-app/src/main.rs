//! Cobalt SQL Works — entry point.
//!

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod commands;
mod copy;
mod fabric;
mod gpu;
mod ops;
mod session;
mod state;
mod update;
mod ui;
#[cfg(feature = "agent")]
mod agent;

/// The window / taskbar icon, decoded from the bundled 256 px PNG (`assets/icon.svg` rendered).
fn app_icon() -> egui::IconData {
    const PNG: &[u8] = include_bytes!("../../../assets/icons/icon_256.png");
    match image::load_from_memory_with_format(PNG, image::ImageFormat::Png) {
        Ok(img) => {
            let rgba = img.into_rgba8();
            let (width, height) = rgba.dimensions();
            egui::IconData { rgba: rgba.into_raw(), width, height }
        }
        Err(e) => {
            tracing::warn!("could not decode the bundled app icon: {e}");
            egui::IconData::default()
        }
    }
}

fn main() -> eframe::Result {
    init_tracing();
    select_gpu_backend();
    // the renderer must be chosen before the window exists; settings are loaded again by the app
    let renderer_setting = cobalt_store::AppPaths::new().map(|p| cobalt_store::load_settings(&p.settings_file()).advanced.renderer).unwrap_or_else(|_| "auto".into());
    let renderer = gpu::choose_renderer(&renderer_setting);

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(cobalt_core::APP_NAME)
            .with_icon(app_icon())
            .with_inner_size(std::env::var("COBALT_WINDOW").ok().and_then(|s| { let (w, h) = s.split_once('x')?; Some([w.parse().ok()?, h.parse().ok()?]) }).unwrap_or([1400.0, 900.0]))
            .with_min_inner_size([800.0, 500.0])
            .with_app_id("cobalt-sqlworks"),
        persist_window: true,
        wgpu_options: {
            // pick the adapter ourselves: best GPU, or WARP with a warning (see gpu.rs)
            let mut o = eframe::egui_wgpu::WgpuConfiguration::default();
            if let eframe::egui_wgpu::WgpuSetup::CreateNew(c) = &mut o.wgpu_setup {
                c.native_adapter_selector = Some(gpu::selector());
            }
            // diagnostics knobs (undocumented): COBALT_PRESENT=fifo|immediate|mailbox|autovsync|autonovsync,
            // COBALT_LATENCY=<frames>
            if let Ok(p) = std::env::var("COBALT_PRESENT") {
                use eframe::egui_wgpu::wgpu::PresentMode as P;
                o.surface.present_mode = match p.to_ascii_lowercase().as_str() {
                    "fifo" => P::Fifo,
                    "immediate" => P::Immediate,
                    "mailbox" => P::Mailbox,
                    "autonovsync" => P::AutoNoVsync,
                    _ => P::AutoVsync,
                };
            }
            if let Some(n) = std::env::var("COBALT_LATENCY").ok().and_then(|s| s.parse::<u32>().ok()) {
                o.surface.desired_maximum_frame_latency = Some(n);
            }
            o
        },
        renderer,
        // COBALT_DITHER=0 turns off egui's per-pixel dithering (a fragment-shader cost that matters on WARP)
        dithering: std::env::var("COBALT_DITHER").map(|v| v != "0").unwrap_or(true),
        ..Default::default()
    };

    #[cfg(feature = "agent")]
    let control = {
        let transport = if let Ok(name) = std::env::var("COBALT_AGENT_PIPE") {
            egui_agent::Transport::Pipe(name)
        } else {
            #[cfg(unix)]
            {
                egui_agent::Transport::UnixSocket(std::env::temp_dir().join("cobalt.agent.sock"))
            }
            #[cfg(not(unix))]
            {
                egui_agent::Transport::Pipe(cobalt_core::AGENT_PIPE_NAME.into())
            }
        };
        egui_agent::Control::spawn(egui_agent::Config {
            transport,
            auth: egui_agent::Auth::from_env("COBALT_AGENT_TOKEN"),
            enabled: std::env::var("COBALT_AGENT").is_ok(),
            ..Default::default()
        })
    };

    eframe::run_native(
        cobalt_core::APP_NAME,
        native_options,
        Box::new(move |cc| {
            let app = app::CobaltApp::new(cc);
            #[cfg(feature = "agent")]
            {
                control.bind_context(cc.egui_ctx.clone());
                Ok(Box::new(egui_agent::wrap(app, control)))
            }
            #[cfg(not(feature = "agent"))]
            {
                Ok(Box::new(app))
            }
        }),
    )
}

/// wgpu's default "enumerate every backend" path crashes on some Windows machines with mixed
/// AMD/NVIDIA Vulkan layers before any Rust code can catch it. Pick one backend explicitly:
/// DX12 on Windows, Metal on macOS, Vulkan elsewhere. `WGPU_BACKEND` (or, later, the
/// `advanced.renderer` setting) overrides.
fn select_gpu_backend() {
    if std::env::var_os("WGPU_BACKEND").is_some() {
        return;
    }
    let backend = if cfg!(target_os = "windows") {
        "dx12"
    } else if cfg!(target_os = "macos") {
        "metal"
    } else {
        "vulkan"
    };
    // SAFETY: called at the very start of main before any other thread exists.
    unsafe { std::env::set_var("WGPU_BACKEND", backend) };
}

fn init_tracing() {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};
    let filter = EnvFilter::try_from_env("COBALT_LOG").unwrap_or_else(|_| EnvFilter::new("info,wgpu=warn,naga=warn,egui_wgpu=warn"));
    // COBALT_SPANS=1: print span close events (durations) — used with `COBALT_LOG=…,eframe=trace,egui_wgpu=trace`
    // to see where a frame's time goes inside the renderer.
    let spans = if std::env::var_os("COBALT_SPANS").is_some() { fmt::format::FmtSpan::CLOSE } else { fmt::format::FmtSpan::NONE };
    tracing_subscriber::registry().with(filter).with(fmt::layer().with_target(true).with_span_events(spans)).init();
}
