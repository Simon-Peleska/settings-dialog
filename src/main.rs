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
mod tasks;
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

    let page = match std::env::args().nth(1) {
        None => app::Page::Network,
        Some(name) => match app::PAGES
            .iter()
            .find(|(_, page)| app::page_name(*page) == name)
        {
            Some((_, page)) => *page,
            None => {
                let names: Vec<String> =
                    app::PAGES.iter().map(|(_, p)| app::page_name(*p)).collect();
                eprintln!(
                    "[ARGS-1] unknown page '{name}', expected one of: {}",
                    names.join(", ")
                );
                std::process::exit(2);
            }
        },
    };

    // A second launch hands its page to the running window and exits.
    let (send, requests) = std::sync::mpsc::channel();
    let _ = send.send(page);
    let wake: std::sync::Arc<std::sync::OnceLock<egui::Context>> = Default::default();
    let handler_wake = wake.clone();
    let claim = fastframe_instance::Slot::new("settings-dialog").claim(
        &app::page_name(page),
        move |name| {
            let page = app::PAGES
                .iter()
                .find(|(_, p)| app::page_name(*p) == name)?
                .1;
            send.send(page).ok()?;
            if let Some(ctx) = handler_wake.get() {
                ctx.request_repaint();
            }
            Some("ok".to_owned())
        },
    );
    let _guard = match claim {
        fastframe_instance::Claim::First(guard) => guard,
        fastframe_instance::Claim::Running(_) => {
            log::info!("handed page {} to the running window", app::page_name(page));
            return Ok(());
        }
        other => {
            log::error!("[ARGS-2] the running window did not take the page: {other:?}");
            eprintln!("[ARGS-2] the running window did not take the page: {other:?}");
            std::process::exit(1);
        }
    };

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
            let _ = wake.set(ctx.clone());
            Ok(Box::new(app::App::new(ctx, pal, requests)))
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
