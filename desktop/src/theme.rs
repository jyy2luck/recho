use eframe::egui::{self, Color32};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum ThemeColor {
    #[default]
    Green,
    Blue,
    Orange,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Appearance {
    #[default]
    System,
    Dark,
    Light,
}

impl Appearance {
    pub(super) const OPTIONS: [(Self, &str); 3] = [
        (Self::System, "跟随系统"),
        (Self::Dark, "深"),
        (Self::Light, "浅"),
    ];
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Theme {
    pub(super) color: ThemeColor,
    pub(super) appearance: Appearance,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SavedTheme {
    Current(Theme),
    Legacy(String),
}

#[derive(Clone, Copy)]
pub(super) struct Palette {
    pub(super) bg: Color32,
    pub(super) side: Color32,
    pub(super) chrome: Color32,
    code: Color32,
    pub(super) line: Color32,
    pub(super) text: Color32,
    pub(super) muted: Color32,
    pub(super) accent: Color32,
    pub(super) selected: Color32,
    pub(super) green: Color32,
    pub(super) red: Color32,
    pub(super) warning: Color32,
    pub(super) key: Color32,
}

fn rgb(value: u32) -> Color32 {
    Color32::from_rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
}

impl ThemeColor {
    pub(super) const OPTIONS: [(Self, &str); 3] = [
        (Self::Green, "绿"),
        (Self::Blue, "蓝"),
        (Self::Orange, "橙"),
    ];

    fn palette(self, appearance: egui::Theme) -> Palette {
        match (self, appearance) {
            (Self::Orange, egui::Theme::Dark) => Palette {
                bg: rgb(0x1e1c1a),
                side: rgb(0x221f1d),
                chrome: rgb(0x26221f),
                code: rgb(0x29241f),
                line: rgb(0x413933),
                text: rgb(0xf3eae0),
                muted: rgb(0xbdb0a3),
                accent: rgb(0xffab79),
                selected: rgb(0x3d2c23),
                green: rgb(0x93d3aa),
                red: rgb(0xff9ea2),
                warning: rgb(0xe2c27d),
                key: rgb(0xaac6f0),
            },
            (Self::Orange, egui::Theme::Light) => Palette {
                bg: rgb(0xfffdfa),
                side: rgb(0xfaf6f0),
                chrome: rgb(0xf7f2eb),
                code: rgb(0xf9f4ee),
                line: rgb(0xded4c8),
                text: rgb(0x2a231d),
                muted: rgb(0x675b50),
                accent: rgb(0x9b4317),
                selected: rgb(0xf4e1d1),
                green: rgb(0x216344),
                red: rgb(0xa32f3e),
                warning: rgb(0x855500),
                key: rgb(0x31588a),
            },
            (Self::Blue, egui::Theme::Dark) => Palette {
                bg: rgb(0x141f30),
                side: rgb(0x192739),
                chrome: rgb(0x182435),
                code: rgb(0x1c2b40),
                line: rgb(0x31445c),
                text: rgb(0xecf4ff),
                muted: rgb(0xa9bfd6),
                accent: rgb(0xa0cdff),
                selected: rgb(0x213c58),
                green: rgb(0x8bd5b4),
                red: rgb(0xff9fb1),
                warning: rgb(0xefc77e),
                key: rgb(0xafcaff),
            },
            (Self::Blue, egui::Theme::Light) => Palette {
                bg: rgb(0xfcfdff),
                side: rgb(0xf7faff),
                chrome: rgb(0xf0f5fa),
                code: rgb(0xf2f6fb),
                line: rgb(0xd1deea),
                text: rgb(0x152d48),
                muted: rgb(0x4d637b),
                accent: rgb(0x1b5295),
                selected: rgb(0xdceaf9),
                green: rgb(0x1c6749),
                red: rgb(0xa4314d),
                warning: rgb(0x7d510b),
                key: rgb(0x305a9a),
            },
            (Self::Green, egui::Theme::Dark) => Palette {
                bg: rgb(0x14221b),
                side: rgb(0x1b2a23),
                chrome: rgb(0x1b2a22),
                code: rgb(0x1a2b23),
                line: rgb(0x354c3e),
                text: rgb(0xebf5ed),
                muted: rgb(0xa8c3b2),
                accent: rgb(0xa6e0b6),
                selected: rgb(0x284536),
                green: rgb(0x9bdab3),
                red: rgb(0xffa4ad),
                warning: rgb(0xecc681),
                key: rgb(0xadc9ef),
            },
            (Self::Green, egui::Theme::Light) => Palette {
                bg: rgb(0xfbfdf9),
                side: rgb(0xf2f7f1),
                chrome: rgb(0xedf4ed),
                code: rgb(0xf5f9f3),
                line: rgb(0xceddce),
                text: rgb(0x1d362b),
                muted: rgb(0x4e6659),
                accent: rgb(0x22634c),
                selected: rgb(0xdcece0),
                green: rgb(0x21654a),
                red: rgb(0xa13043),
                warning: rgb(0x7e5108),
                key: rgb(0x325a85),
            },
        }
    }
}

impl Theme {
    pub(super) fn palette(self, ctx: &egui::Context) -> Palette {
        self.color.palette(ctx.theme())
    }

    pub(super) fn current_palette(ctx: &egui::Context) -> Palette {
        let color: ThemeColor = ctx
            .data(|data| data.get_temp(egui::Id::new("recho-theme-color")))
            .unwrap_or_default();
        color.palette(ctx.theme())
    }

    pub(super) fn apply(self, ctx: &egui::Context) {
        ctx.set_theme(match self.appearance {
            Appearance::System => egui::ThemePreference::System,
            Appearance::Dark => egui::ThemePreference::Dark,
            Appearance::Light => egui::ThemePreference::Light,
        });
        ctx.data_mut(|data| data.insert_temp(egui::Id::new("recho-theme-color"), self.color));
        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            let p = self.color.palette(theme);
            ctx.style_mut_of(theme, |style| {
                style.animation_time = 0.0;
                style.spacing.item_spacing = egui::vec2(6.0, 4.0);
                style.spacing.button_padding = egui::vec2(7.0, 4.0);
                let visuals = &mut style.visuals;
                visuals.panel_fill = p.bg;
                visuals.window_fill = p.side;
                visuals.extreme_bg_color = p.bg;
                visuals.faint_bg_color = p.side;
                visuals.code_bg_color = p.code;
                visuals.window_stroke = egui::Stroke::new(1.0, p.line);
                visuals.selection.bg_fill = p.selected;
                visuals.selection.stroke.color = p.accent;
                visuals.text_cursor.stroke.color = p.accent;
                visuals.override_text_color = Some(p.text);
                visuals.weak_text_color = Some(p.muted);
                visuals.hyperlink_color = p.accent;
                visuals.warn_fg_color = p.warning;
                visuals.error_fg_color = p.red;
                let widgets = &mut visuals.widgets;
                for (visual, fill, stroke) in [
                    (&mut widgets.noninteractive, p.side, p.line),
                    (&mut widgets.inactive, p.chrome, p.line),
                    (&mut widgets.hovered, p.selected, p.accent),
                    (&mut widgets.active, p.selected, p.accent),
                    (&mut widgets.open, p.selected, p.accent),
                ] {
                    visual.bg_fill = fill;
                    visual.weak_bg_fill = fill;
                    visual.bg_stroke.color = stroke;
                    visual.fg_stroke.color = p.text;
                }
            });
        }
        ctx.request_repaint();
    }

    pub(super) fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read(path) {
            Ok(bytes) => match serde_json::from_slice(&bytes)
                .map_err(|error| format!("主题设置读取失败：{error}"))?
            {
                SavedTheme::Current(theme) => Ok(theme),
                SavedTheme::Legacy(name) => {
                    use Appearance::{Dark, Light};
                    use ThemeColor::{Blue, Green, Orange};
                    let (color, appearance) = match name.as_str() {
                        "green-dark" => (Green, Dark),
                        "green-light" => (Green, Light),
                        "blue-dark" => (Blue, Dark),
                        "blue-light" => (Blue, Light),
                        "orange-dark" => (Orange, Dark),
                        "orange-light" => (Orange, Light),
                        _ => return Err(format!("主题设置读取失败：未知主题 {name}")),
                    };
                    Ok(Self { color, appearance })
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(format!("主题设置读取失败：{error}")),
        }
    }

    pub(super) fn save(self, path: &Path) -> Result<(), String> {
        let bytes = serde_json::to_vec(&self).map_err(|error| error.to_string())?;
        recho_core::native::write_local_file(path, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_and_legacy_preferences_load_and_round_trip() {
        use Appearance::{Dark, Light, System};
        use ThemeColor::{Blue, Green, Orange};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("desktop-theme.json");
        let default = Theme {
            color: Green,
            appearance: System,
        };
        assert_eq!(Theme::load(&path).unwrap(), default);

        for (legacy, color, appearance) in [
            ("green-dark", Green, Dark),
            ("green-light", Green, Light),
            ("blue-dark", Blue, Dark),
            ("blue-light", Blue, Light),
            ("orange-dark", Orange, Dark),
            ("orange-light", Orange, Light),
        ] {
            let expected = Theme { color, appearance };
            std::fs::write(&path, serde_json::to_vec(legacy).unwrap()).unwrap();
            assert_eq!(Theme::load(&path).unwrap(), expected);
            expected.save(&path).unwrap();
            assert_eq!(Theme::load(&path).unwrap(), expected);
        }

        default.save(&path).unwrap();
        assert_eq!(Theme::load(&path).unwrap(), default);
    }

    #[test]
    fn current_palette_tracks_system_only_in_system_mode() {
        use egui::Theme::{Dark, Light};

        let ctx = egui::Context::default();
        let mut theme = Theme::default();
        for (color, _) in ThemeColor::OPTIONS {
            theme.color = color;
            for (appearance, fixed) in [
                (Appearance::System, None),
                (Appearance::Dark, Some(Dark)),
                (Appearance::Light, Some(Light)),
            ] {
                theme.appearance = appearance;
                theme.apply(&ctx);
                for system_theme in [Light, Dark, Light] {
                    let mut output = ctx.run_ui(
                        egui::RawInput {
                            system_theme: Some(system_theme),
                            ..Default::default()
                        },
                        |ui| {
                            let expected = color.palette(fixed.unwrap_or(system_theme));
                            assert_eq!(ui.visuals().panel_fill, expected.bg);
                            assert_eq!(ui.visuals().code_bg_color, expected.code);
                            assert_eq!(theme.palette(&ctx).accent, expected.accent);
                            assert_eq!(Theme::current_palette(&ctx).accent, expected.accent);
                        },
                    );
                    output.textures_delta.clear();
                }
            }
        }
    }
}
