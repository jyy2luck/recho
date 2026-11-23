use eframe::egui::{self, Color32};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Theme {
    #[default]
    OrangeDark,
    OrangeLight,
    BlueDark,
    BlueLight,
    GreenDark,
    GreenLight,
}

#[derive(Clone, Copy)]
pub(super) struct Palette {
    pub(super) bg: Color32,
    pub(super) side: Color32,
    pub(super) chrome: Color32,
    pub(super) line: Color32,
    pub(super) text: Color32,
    pub(super) muted: Color32,
    pub(super) accent: Color32,
    pub(super) selected: Color32,
    pub(super) green: Color32,
    pub(super) red: Color32,
    pub(super) warning: Color32,
    pub(super) key: Color32,
    pub(super) string: Color32,
}

fn rgb(value: u32) -> Color32 {
    Color32::from_rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
}

impl Theme {
    pub(super) const ALL: [Self; 6] = [
        Self::OrangeDark,
        Self::OrangeLight,
        Self::BlueDark,
        Self::BlueLight,
        Self::GreenDark,
        Self::GreenLight,
    ];

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::OrangeDark => "炭黑橙",
            Self::OrangeLight => "暖白橙",
            Self::BlueDark => "午夜蓝",
            Self::BlueLight => "雾白蓝",
            Self::GreenDark => "松林绿",
            Self::GreenLight => "瓷白绿",
        }
    }

    pub(super) fn palette(self) -> Palette {
        match self {
            Self::OrangeDark => Palette {
                bg: rgb(0x191b20),
                side: rgb(0x1f2228),
                chrome: rgb(0x272a31),
                line: rgb(0x393e48),
                text: rgb(0xe6e8ed),
                muted: rgb(0xa5adbb),
                accent: rgb(0xff9e64),
                selected: rgb(0x3d2c24),
                green: rgb(0x83d4a5),
                red: rgb(0xff939b),
                warning: rgb(0xe6bd72),
                key: rgb(0xa8c5f4),
                string: rgb(0x9bd2b0),
            },
            Self::OrangeLight => Palette {
                bg: rgb(0xfffefc),
                side: rgb(0xf8f4ef),
                chrome: rgb(0xf0e9e0),
                line: rgb(0xdfd5c9),
                text: rgb(0x372c25),
                muted: rgb(0x756356),
                accent: rgb(0xa7460e),
                selected: rgb(0xf9e7d7),
                green: rgb(0x237341),
                red: rgb(0xb63340),
                warning: rgb(0x8b5b11),
                key: rgb(0x3b6095),
                string: rgb(0x287347),
            },
            Self::BlueDark => Palette {
                bg: rgb(0x111b28),
                side: rgb(0x172332),
                chrome: rgb(0x1d2c3e),
                line: rgb(0x34465c),
                text: rgb(0xe4edf8),
                muted: rgb(0xa3b4cc),
                accent: rgb(0x7ab9ff),
                selected: rgb(0x243d5b),
                green: rgb(0x88d5b2),
                red: rgb(0xff969f),
                warning: rgb(0xeac385),
                key: rgb(0xa0beff),
                string: rgb(0x98d6bd),
            },
            Self::BlueLight => Palette {
                bg: rgb(0xffffff),
                side: rgb(0xf2f5fa),
                chrome: rgb(0xe8eef6),
                line: rgb(0xd5dee9),
                text: rgb(0x233044),
                muted: rgb(0x586a82),
                accent: rgb(0x245fc0),
                selected: rgb(0xe4edfd),
                green: rgb(0x197345),
                red: rgb(0xb93845),
                warning: rgb(0x8e5a13),
                key: rgb(0x3c5eb0),
                string: rgb(0x1a7453),
            },
            Self::GreenDark => Palette {
                bg: rgb(0x121e1b),
                side: rgb(0x182822),
                chrome: rgb(0x20332b),
                line: rgb(0x354d41),
                text: rgb(0xe2ede6),
                muted: rgb(0xa6bcae),
                accent: rgb(0x88d5ad),
                selected: rgb(0x294638),
                green: rgb(0x88d5ad),
                red: rgb(0xffa2a3),
                warning: rgb(0xe2c082),
                key: rgb(0xa2c8e6),
                string: rgb(0xa7d5b9),
            },
            Self::GreenLight => Palette {
                bg: rgb(0xfdfefc),
                side: rgb(0xf1f5f0),
                chrome: rgb(0xe7efe6),
                line: rgb(0xd2dfd1),
                text: rgb(0x263a2d),
                muted: rgb(0x566b5c),
                accent: rgb(0x1b704b),
                selected: rgb(0xdfeee1),
                green: rgb(0x1b704b),
                red: rgb(0xb83642),
                warning: rgb(0x8b5b15),
                key: rgb(0x376b97),
                string: rgb(0x287449),
            },
        }
    }

    pub(super) fn current(ctx: &egui::Context) -> Self {
        ctx.data_mut(|data| data.get_temp(egui::Id::new("recho-theme")))
            .unwrap_or_default()
    }

    pub(super) fn apply(self, ctx: &egui::Context) {
        let theme = match self {
            Self::OrangeDark | Self::BlueDark | Self::GreenDark => egui::Theme::Dark,
            _ => egui::Theme::Light,
        };
        ctx.data_mut(|data| data.insert_temp(egui::Id::new("recho-theme"), self));
        ctx.set_theme(theme);
        let p = self.palette();
        ctx.style_mut_of(theme, |style| {
            style.animation_time = 0.0;
            style.spacing.item_spacing = egui::vec2(6.0, 4.0);
            style.spacing.button_padding = egui::vec2(7.0, 4.0);
            let visuals = &mut style.visuals;
            visuals.panel_fill = p.bg;
            visuals.window_fill = p.side;
            visuals.extreme_bg_color = p.bg;
            visuals.faint_bg_color = p.side;
            visuals.code_bg_color = p.side;
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

    pub(super) fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read(path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|error| format!("主题设置读取失败：{error}"))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(format!("主题设置读取失败：{error}")),
        }
    }

    pub(super) fn save(self, path: &Path) -> Result<(), String> {
        let bytes = serde_json::to_vec(&self).map_err(|error| error.to_string())?;
        recho_core::native::write_local_file(path, &bytes)
    }
}
