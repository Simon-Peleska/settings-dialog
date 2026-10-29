mod app;
mod bt;
mod finder;
mod form;
mod hypr;
mod lan;
mod light;
mod nix;
mod nm;
mod pw;
mod screens;
mod tablet;
mod theme;

fn main() -> eframe::Result {
    let mut logging = fastframe_log::Logging::new("settings-dialog", env!("CARGO_PKG_VERSION"))
        .filter("warn,settings_dialog=debug");
    if let Some(state) = state_dir() {
        logging = logging
            .file(state.join("settings-dialog.log"))
            .panic_log(state.join("panics.log"));
    }
    let _ = logging.init();

    let pal = theme::Palette::load();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Settings")
            .with_app_id("settings-dialog")
            .with_inner_size([1360.0, 720.0])
            .with_min_inner_size([1200.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "settings-dialog",
        options,
        Box::new(move |cc| {
            let ctx = &cc.egui_ctx;
            let mut fonts = fastframe_fonts::FontSetup::default().definitions();
            let rendering = fastframe_text::detect();
            rendering.apply_to(&mut fonts);
            ctx.set_fonts(fonts);
            ctx.set_theme(if pal.dark {
                egui::Theme::Dark
            } else {
                egui::Theme::Light
            });
            ctx.all_styles_mut(|style| pal.apply(&mut style.visuals));
            ctx.all_styles_mut(|style| rendering.apply_to_visuals(&mut style.visuals));
            egui_extras::install_image_loaders(ctx);
            fastframe_icons::install::<form::Icon>(ctx);
            Ok(Box::new(app::App::new(ctx, pal)))
        }),
    )
}

fn state_dir() -> Option<std::path::PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".local/state"))
        })?;
    Some(state.join("settings-dialog"))
}
