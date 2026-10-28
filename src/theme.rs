use std::collections::BTreeSet;
use std::path::PathBuf;

use egui::{Color32, CornerRadius, Shadow, Stroke, Visuals};

#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    pub dark: bool,
    pub window: Color32,
    pub panel: Color32,
    pub surface: Color32,
    pub surface_hover: Color32,
    pub surface_active: Color32,
    pub outline: Color32,
    pub text: Color32,
    pub secondary: Color32,
    pub dim: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub on_accent: Color32,
    pub danger: Color32,
    pub warning: Color32,
    pub overlay: Color32,
    pub shadow: Color32,
    pub success: Color32,
    pub info: Color32,
}

const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

impl Palette {
    pub fn gruvbox_dark_hard() -> Self {
        Self {
            dark: true,
            window: hex(0x1d2021),
            panel: hex(0x282828),
            surface: hex(0x3c3836),
            surface_hover: hex(0x504945),
            surface_active: hex(0x665c54),
            outline: hex(0x504945),
            text: hex(0xebdbb2),
            secondary: hex(0xbdae93),
            dim: hex(0x928374),
            accent: hex(0xfabd2f),
            accent_hover: hex(0xd79921),
            on_accent: hex(0x1d2021),
            danger: hex(0xfb4934),
            warning: hex(0xfe8019),
            overlay: Color32::from_rgba_unmultiplied(0x1d, 0x20, 0x21, 0xb3),
            shadow: Color32::from_black_alpha(0x80),
            success: hex(0xb8bb26),
            info: hex(0x83a598),
        }
    }

    pub fn gruvbox_light_hard() -> Self {
        Self {
            dark: false,
            window: hex(0xf9f5d7),
            panel: hex(0xfbf1c7),
            surface: hex(0xebdbb2),
            surface_hover: hex(0xd5c4a1),
            surface_active: hex(0xbdae93),
            outline: hex(0xd5c4a1),
            text: hex(0x3c3836),
            secondary: hex(0x665c54),
            dim: hex(0x928374),
            accent: hex(0xb57614),
            accent_hover: hex(0xd79921),
            on_accent: hex(0xf9f5d7),
            danger: hex(0x9d0006),
            warning: hex(0xaf3a03),
            overlay: Color32::from_rgba_unmultiplied(0xf9, 0xf5, 0xd7, 0xb3),
            shadow: Color32::from_black_alpha(0x40),
            success: hex(0x79740e),
            info: hex(0x076678),
        }
    }

    pub fn load() -> Self {
        let Some(path) = theme_path() else {
            return Self::gruvbox_dark_hard();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => fastframe_theme::parse_palette(&text).unwrap_or_else(|err| {
                log::warn!("ignoring {}: {err}", path.display());
                Self::gruvbox_dark_hard()
            }),
            Err(_) => Self::gruvbox_dark_hard(),
        }
    }

    pub fn apply(&self, visuals: &mut Visuals) {
        *visuals = if self.dark {
            Visuals::dark()
        } else {
            Visuals::light()
        };
        visuals.override_text_color = Some(self.text);
        visuals.weak_text_color = Some(self.dim);
        visuals.panel_fill = self.window;
        visuals.window_fill = self.panel;
        visuals.window_stroke = Stroke::new(1.0, self.outline);
        visuals.window_shadow = Shadow {
            color: self.shadow,
            ..Shadow::NONE
        };
        visuals.popup_shadow = Shadow {
            color: self.shadow,
            ..Shadow::NONE
        };
        visuals.window_corner_radius = CornerRadius::same(4);
        visuals.menu_corner_radius = CornerRadius::same(4);
        visuals.extreme_bg_color = self.window;
        visuals.text_edit_bg_color = Some(self.window);
        visuals.faint_bg_color = self.surface;
        visuals.code_bg_color = self.surface;
        visuals.hyperlink_color = self.info;
        visuals.warn_fg_color = self.warning;
        visuals.error_fg_color = self.danger;
        visuals.selection.bg_fill = self.accent_hover;
        visuals.selection.stroke = Stroke::new(1.0, self.on_accent);
        visuals.text_cursor.stroke = Stroke::new(2.0, self.accent);

        let widgets = &mut visuals.widgets;
        widgets.noninteractive.bg_fill = self.panel;
        widgets.noninteractive.weak_bg_fill = self.panel;
        widgets.noninteractive.bg_stroke = Stroke::new(1.0, self.outline);
        widgets.noninteractive.fg_stroke = Stroke::new(1.0, self.text);
        for (state, fill) in [
            (&mut widgets.inactive, self.surface),
            (&mut widgets.hovered, self.surface_hover),
            (&mut widgets.active, self.surface_active),
            (&mut widgets.open, self.surface_active),
        ] {
            state.bg_fill = fill;
            state.weak_bg_fill = fill;
            state.bg_stroke = Stroke::new(1.0, self.outline);
            state.fg_stroke = Stroke::new(1.0, self.text);
        }
    }
}

fn theme_path() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(config.join("settings-dialog").join("theme.json"))
}

impl fastframe_theme::Palette for Palette {
    fn base(base: fastframe_theme::Base) -> Self {
        match base {
            fastframe_theme::Base::Dark => Self::gruvbox_dark_hard(),
            fastframe_theme::Base::Light => Self::gruvbox_light_hard(),
        }
    }

    fn set(&mut self, name: &str, color: Color32) -> bool {
        let slot = match name {
            "window" => &mut self.window,
            "panel" => &mut self.panel,
            "surface" => &mut self.surface,
            "surface_hover" => &mut self.surface_hover,
            "surface_active" => &mut self.surface_active,
            "outline" => &mut self.outline,
            "text" => &mut self.text,
            "secondary" => &mut self.secondary,
            "dim" => &mut self.dim,
            "accent" => &mut self.accent,
            "accent_hover" => &mut self.accent_hover,
            "on_accent" => &mut self.on_accent,
            "danger" => &mut self.danger,
            "warning" => &mut self.warning,
            "overlay" => &mut self.overlay,
            "shadow" => &mut self.shadow,
            "success" => &mut self.success,
            "info" => &mut self.info,
            _ => return false,
        };
        *slot = color;
        true
    }

    fn derive(&mut self, given: &BTreeSet<&str>) {
        // Shared palettes only carry the sixteen base colours.
        if given.contains("accent") && !given.contains("info") {
            self.info = self.secondary;
        }
    }
}

impl fastframe_theme::PaletteBase for Palette {
    fn palette_base(&self) -> fastframe_theme::Base {
        fastframe_theme::Base::from_dark(self.dark)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_palette_file_is_the_default() {
        let file: Palette =
            fastframe_theme::parse_palette(include_str!("../themes/Gruvbox Dark Hard.json"))
                .unwrap();
        assert_eq!(file, Palette::gruvbox_dark_hard());
    }
}
