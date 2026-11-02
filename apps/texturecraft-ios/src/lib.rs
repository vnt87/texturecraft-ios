//! TextureCraft's native iPadOS runner: the Rust static library the Xcode harness in
//! `packaging/ios` links into an app bundle.
//!
//! UIKit owns the window scene, winit/eframe own the Metal surface, and the egui shell is the same
//! `TextureCraftApp` the desktop and browser builds run. Anything Apple-specific stays inside the
//! audited bridge modules (`apple_entry`, `scenes`, `files`, `pencil`).
#![deny(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(target_os = "ios")]
#[allow(unsafe_code)]
mod apple_entry;
/// UIKit document pickers: open answers with file bytes, export hands a finished file to the user.
/// The name and size rules are platform-free and host-tested.
#[cfg(any(target_os = "ios", test))]
mod files;
#[cfg(any(target_os = "ios", test))]
mod logging;
/// Apple Pencil input: the audited UIKit boundary that tells the shell which touch is the pen and
/// that the pencil was double-tapped.
#[cfg(target_os = "ios")]
#[allow(unsafe_code)]
mod pencil;
#[cfg(target_os = "ios")]
mod safe_area;
#[cfg(target_os = "ios")]
#[allow(unsafe_code)]
mod scenes;

/// The app id eframe and UIKit keep settings and containers under.
pub const APP_ID: &str = "ai.storyteller.texturecraft";

/// The app name in window titles and logs; the product name stays PascalCase.
pub const APP_NAME: &str = "TextureCraft";

/// Configure Metal explicitly; UIKit/winit supplies the current window dimensions.
pub fn native_options() -> eframe::NativeOptions {
    let mut setup = eframe::egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.instance_descriptor.backends = eframe::wgpu::Backends::METAL;
    setup.device_descriptor = std::sync::Arc::new(|adapter| {
        let available = adapter.limits();
        eframe::wgpu::DeviceDescriptor {
            label: Some("texturecraft iOS Metal device"),
            required_limits: compatible_limits(eframe::wgpu::Limits::default(), &available),
            ..Default::default()
        }
    });
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_app_id(APP_ID).with_title(APP_NAME),
        renderer: eframe::Renderer::Wgpu,
        run_and_return: false,
        persistence_path: persistence_path(),
        wgpu_options: eframe::WgpuConfiguration { wgpu_setup: eframe::egui_wgpu::WgpuSetup::CreateNew(setup), ..Default::default() },
        ..Default::default()
    }
}

/// iOS gives eframe no data directory, which disables its key/value store and so silently drops
/// every preference at quit; name the file inside the app container instead.
#[cfg(target_os = "ios")]
fn persistence_path() -> Option<std::path::PathBuf> {
    Some(files::support_dir(std::env::var_os("HOME").as_deref()).ok()?.join("ui.ron"))
}

/// Elsewhere eframe knows the OS's own data directory.
#[cfg(not(target_os = "ios"))]
fn persistence_path() -> Option<std::path::PathBuf> {
    None
}

/// egui uses two inter-stage locations; Metal simulators may expose 15 instead of wgpu's default
/// 16. Keep every other limit the editor asked for.
fn compatible_limits(mut requested: eframe::wgpu::Limits, available: &eframe::wgpu::Limits) -> eframe::wgpu::Limits {
    // Below two, device creation must fail rather than pretend the egui shader is supported.
    requested.max_inter_stage_shader_variables = requested.max_inter_stage_shader_variables.min(available.max_inter_stage_shader_variables).max(2);
    requested
}

/// The document the app opens with: the sample project the desktop shows on a fresh run, or an
/// empty material ball if the sample cannot be built.
#[cfg(any(target_os = "ios", test))]
fn initial_session() -> Result<texturecraft_engine::Session, String> {
    if let Ok(sample) = texturecraft_engine::sample_session(2048, 512) {
        return Ok(sample);
    }
    texturecraft_engine::Session::new_project("Untitled", texturecraft_engine::geom::primitives::material_ball(), "builtin:material_ball", None, 1024)
        .map_err(|error| format!("could not open a first project: {error}"))
}

/// Launch on iOS's main thread; other platforms return an error without a window.
pub fn run() -> Result<(), String> {
    #[cfg(target_os = "ios")]
    {
        if let Err(error) = logging::install() {
            eprintln!("{APP_NAME} logging unavailable: {error}");
        }
        log::info!("Starting {APP_NAME} iPadOS with Metal");
        scenes::register();
        // The shell asks these hooks for pickers; without them, file actions fail with a message.
        files::install();
        eframe::run_native(
            APP_NAME,
            native_options(),
            Box::new(|cc| {
                scenes::attach(cc)?;
                // Hide the status bar through winit's safe API so it cannot overlay the menu bar.
                use winit::platform::ios::WindowExtIOS;
                if let Some(window) = cc.winit_window() {
                    window.set_prefers_status_bar_hidden(true);
                }
                let session = initial_session()?;
                let app = texturecraft_ui_egui::TextureCraftApp::new(cc, session);
                // Apple Pencil: the pen's touch ids and its double-tap (winit forwards only touch force).
                if let Err(error) = pencil::install(&cc.egui_ctx) {
                    log::error!("Apple Pencil input unavailable: {error}");
                }
                Ok(Box::new(safe_area::SafeAreaProbe::new(app)))
            }),
        )
        .map_err(|error| format!("{APP_NAME} iOS startup failed: {error}"))
    }
    #[cfg(not(target_os = "ios"))]
    {
        Err("the TextureCraft iOS runner needs an iOS device or simulator".into())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn simulator_varying_limit_does_not_reduce_other_editor_limits() {
        let requested = eframe::wgpu::Limits { max_texture_dimension_2d: 16384, max_buffer_size: 512 * 1024 * 1024, ..Default::default() };
        let available = eframe::wgpu::Limits { max_inter_stage_shader_variables: 15, ..requested.clone() };
        let actual = super::compatible_limits(requested.clone(), &available);
        assert_eq!(actual, eframe::wgpu::Limits { max_inter_stage_shader_variables: 15, ..requested });
    }

    #[test]
    fn adapters_below_egui_minimum_cannot_silently_lower_shader_requirements() {
        let available = eframe::wgpu::Limits { max_inter_stage_shader_variables: 1, ..Default::default() };
        assert_eq!(super::compatible_limits(eframe::wgpu::Limits::default(), &available).max_inter_stage_shader_variables, 2);
    }

    #[test]
    fn viewport_uses_os_size_and_only_metal() {
        let options = super::native_options();
        assert_eq!(options.renderer, eframe::Renderer::Wgpu);
        assert_eq!(options.viewport.app_id.as_deref(), Some(super::APP_ID));
        assert!(options.viewport.inner_size.is_none());
        assert!(!options.run_and_return);
        match options.wgpu_options.wgpu_setup {
            eframe::egui_wgpu::WgpuSetup::CreateNew(setup) => assert_eq!(setup.instance_descriptor.backends, eframe::wgpu::Backends::METAL),
            _ => panic!("the runner must create its own Metal device"),
        }
    }

    #[test]
    fn runner_rejects_non_ios_hosts() {
        if !cfg!(target_os = "ios") {
            assert!(super::run().unwrap_err().contains("iOS"));
        }
    }

    #[test]
    fn a_first_project_always_opens() {
        super::initial_session().expect("the sample project or an empty material ball");
    }
}
