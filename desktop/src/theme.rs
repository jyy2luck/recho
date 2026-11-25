use eframe::egui::{self, Color32};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum ThemeColor {
    #[default]
    Monochrome,
    Green,
    Blue,
    Purple,
    Berry,
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

pub(super) const CONTROL_HEIGHT: f32 = 32.0;

impl Palette {
    pub(super) fn input_frame(self) -> egui::Frame {
        egui::Frame::new()
            .fill(self.bg)
            .stroke(egui::Stroke::new(1.0, self.line))
            .corner_radius(5)
    }
}

pub(super) fn input_text<'a>(
    value: &'a mut String,
    label: &'static str,
    width: f32,
) -> egui::TextEdit<'a> {
    egui::TextEdit::singleline(value)
        .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(8, 0)))
        .vertical_align(egui::Align::Center)
        .hint_text(label)
        .min_size(egui::vec2(width, CONTROL_HEIGHT - 2.0))
        .desired_width(width)
}

fn rgb(value: u32) -> Color32 {
    Color32::from_rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
}

impl ThemeColor {
    pub(super) const OPTIONS: [(Self, &str); 6] = [
        (Self::Monochrome, "黑白"),
        (Self::Green, "绿"),
        (Self::Blue, "蓝"),
        (Self::Purple, "紫色"),
        (Self::Berry, "莓红"),
        (Self::Orange, "橙"),
    ];

    fn palette(self, appearance: egui::Theme) -> Palette {
        match (self, appearance) {
            (Self::Monochrome, egui::Theme::Dark) => Palette {
                bg: rgb(0x121212),
                side: rgb(0x191919),
                chrome: rgb(0x212121),
                code: rgb(0x1c1c1c),
                line: rgb(0x383838),
                text: rgb(0xe8e8e8),
                muted: rgb(0xb5b5b5),
                accent: rgb(0xf9f9f9),
                selected: rgb(0x323232),
                green: rgb(0x98d8ae),
                red: rgb(0xff9498),
                warning: rgb(0xeac681),
                key: rgb(0xadcbf5),
            },
            (Self::Monochrome, egui::Theme::Light) => Palette {
                bg: rgb(0xfcfcfc),
                side: rgb(0xf4f4f4),
                chrome: rgb(0xeeeeee),
                code: rgb(0xf7f7f7),
                line: rgb(0xd5d5d5),
                text: rgb(0x262626),
                muted: rgb(0x575757),
                accent: rgb(0x121212),
                selected: rgb(0xe2e2e2),
                green: rgb(0x216648),
                red: rgb(0xa52c37),
                warning: rgb(0x795100),
                key: rgb(0x31578b),
            },
            (Self::Purple, egui::Theme::Dark) => Palette {
                bg: rgb(0x191620),
                side: rgb(0x211c2a),
                chrome: rgb(0x261f31),
                code: rgb(0x241d2e),
                line: rgb(0x3f354f),
                text: rgb(0xf1edf8),
                muted: rgb(0xbfb4cf),
                accent: rgb(0xc7b4f2),
                selected: rgb(0x392c53),
                green: rgb(0x98d8ae),
                red: rgb(0xff9498),
                warning: rgb(0xeac681),
                key: rgb(0xadcbf5),
            },
            (Self::Purple, egui::Theme::Light) => Palette {
                bg: rgb(0xfbfafe),
                side: rgb(0xf5f2fa),
                chrome: rgb(0xeeecf7),
                code: rgb(0xf3f0f9),
                line: rgb(0xdad2e7),
                text: rgb(0x2a2237),
                muted: rgb(0x5f546d),
                accent: rgb(0x6542a4),
                selected: rgb(0xe7ddf5),
                green: rgb(0x216648),
                red: rgb(0xa52c37),
                warning: rgb(0x795100),
                key: rgb(0x31578b),
            },
            (Self::Berry, egui::Theme::Dark) => Palette {
                bg: rgb(0x1d1518),
                side: rgb(0x251a1f),
                chrome: rgb(0x2b1e24),
                code: rgb(0x291d22),
                line: rgb(0x4b343f),
                text: rgb(0xf8edf0),
                muted: rgb(0xcbb6be),
                accent: rgb(0xe8a4ba),
                selected: rgb(0x452b36),
                green: rgb(0x98d8ae),
                red: rgb(0xff9498),
                warning: rgb(0xeac681),
                key: rgb(0xadcbf5),
            },
            (Self::Berry, egui::Theme::Light) => Palette {
                bg: rgb(0xfdf9fa),
                side: rgb(0xf8f0f3),
                chrome: rgb(0xf2e8ec),
                code: rgb(0xf8eef1),
                line: rgb(0xe1cfd6),
                text: rgb(0x33232b),
                muted: rgb(0x685560),
                accent: rgb(0x8a3558),
                selected: rgb(0xf0dbe4),
                green: rgb(0x216648),
                red: rgb(0xa52c37),
                warning: rgb(0x795100),
                key: rgb(0x31578b),
            },
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

    pub(super) fn modal(ctx: &egui::Context, id: egui::Id) -> egui::Modal {
        egui::Modal::new(id).frame(egui::Frame::window(&ctx.global_style()))
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
                style.spacing.window_margin = egui::Margin::same(18);
                let visuals = &mut style.visuals;
                visuals.panel_fill = p.bg;
                visuals.window_fill = p.bg;
                visuals.window_corner_radius = egui::CornerRadius::same(10);
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
        use ThemeColor::{Blue, Green, Monochrome, Orange};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("desktop-theme.json");
        let default = Theme {
            color: Monochrome,
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

        for (color, _) in ThemeColor::OPTIONS {
            for (appearance, _) in Appearance::OPTIONS {
                let expected = Theme { color, appearance };
                expected.save(&path).unwrap();
                assert_eq!(Theme::load(&path).unwrap(), expected);
            }
        }
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
