#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod body;
mod workbench;
mod worker;

use body::{Content, hex_row};
use eframe::egui::{self, Color32, RichText};
use recho_core::native::{Detail, Phase, Row};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};
use worker::{Command, Worker};

const ACCENT: Color32 = Color32::from_rgb(255, 158, 100);
const BG: Color32 = Color32::from_rgb(25, 27, 32);
const SIDE: Color32 = Color32::from_rgb(31, 34, 40);
const CHROME: Color32 = Color32::from_rgb(39, 42, 49);

struct Viewer {
    detail: Detail,
    content: Content,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum MessageTab {
    Overview,
    Raw,
    Headers,
    Body,
}
struct MessagePanel {
    tab: MessageTab,
    hex: bool,
    viewer: Option<Viewer>,
}
impl MessagePanel {
    fn new(tab: MessageTab) -> Self {
        Self {
            tab,
            hex: false,
            viewer: None,
        }
    }
}

struct Recho {
    worker: Worker,
    ready: bool,
    ca_path: PathBuf,
    running: bool,
    intercept_tls: bool,
    port: u16,
    port_editor: String,
    rows: Vec<Row>,
    visible: Vec<usize>,
    sequence: BTreeMap<String, usize>,
    next_sequence: usize,
    filter: String,
    host_filter: String,
    app_filter: String,
    protocol_filter: String,
    status_filter: u16,
    type_filter: String,
    sort_column: usize,
    ascending: bool,
    selected: Option<String>,
    generation: u64,
    messages: [MessagePanel; 2],
    details_open: bool,
    sidebar_visible: bool,
    tools_sidebar: bool,
    connection_open: bool,
    clear_confirmation: bool,
    error: Option<String>,
    history_error: Option<String>,
}
impl Recho {
    fn new(cc: &eframe::CreationContext<'_>, directory: PathBuf, port: u16) -> Self {
        cc.egui_ctx.set_theme(egui::Theme::Dark);
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "cjk".into(),
            std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
                "../assets/fonts/DroidSansFallback.ttf"
            ))),
        );
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push("cjk".into());
        }
        fonts.font_data.insert(
            "icons".into(),
            std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
                "../assets/fonts/MaterialIcons-Regular.ttf"
            ))),
        );
        fonts
            .families
            .insert(egui::FontFamily::Name("icons".into()), vec!["icons".into()]);
        cc.egui_ctx.set_fonts(fonts);
        cc.egui_ctx.style_mut_of(egui::Theme::Dark, |style| {
            style.animation_time = 0.0;
            style.spacing.item_spacing = egui::vec2(6.0, 4.0);
            style.spacing.button_padding = egui::vec2(7.0, 4.0);
            style.visuals.panel_fill = BG;
            style.visuals.window_fill = SIDE;
            style.visuals.extreme_bg_color = BG;
            style.visuals.selection.bg_fill = Color32::from_rgb(61, 44, 36);
            style.visuals.selection.stroke.color = ACCENT;
            style.visuals.override_text_color = Some(Color32::from_rgb(230, 232, 237));
        });
        Self {
            worker: Worker::start(directory, port, cc.egui_ctx.clone()),
            ready: false,
            ca_path: PathBuf::new(),
            running: false,
            intercept_tls: false,
            port: 0,
            port_editor: String::new(),
            rows: Vec::new(),
            visible: Vec::new(),
            sequence: BTreeMap::new(),
            next_sequence: 1,
            filter: String::new(),
            host_filter: String::new(),
            app_filter: String::new(),
            protocol_filter: String::new(),
            status_filter: 0,
            type_filter: String::new(),
            sort_column: 0,
            ascending: true,
            selected: None,
            generation: 0,
            messages: [
                MessagePanel::new(MessageTab::Overview),
                MessagePanel::new(MessageTab::Body),
            ],
            details_open: false,
            sidebar_visible: true,
            tools_sidebar: false,
            connection_open: false,
            clear_confirmation: false,
            error: None,
            history_error: None,
        }
    }

    fn send(&mut self, command: Command) {
        if let Err(error) = self.worker.send(command) {
            self.error = Some(error);
        }
    }
    fn toggle_capture(&mut self) {
        self.send(Command::Configure {
            running: !self.running,
            intercept_tls: self.intercept_tls,
        });
    }
    fn select(&mut self, id: Option<String>) {
        self.selected = id;
        self.generation += 1;
        for message in &mut self.messages {
            message.viewer = None;
        }
        if self.details_open {
            self.load_details();
        }
    }
    fn load_details(&mut self) {
        if let Some(id) = self.selected.clone() {
            for phase in [Phase::Request, Phase::Response] {
                self.send(Command::Detail {
                    generation: self.generation,
                    id: id.clone(),
                    phase,
                });
            }
        }
    }
    fn refilter(&mut self) {
        let query = self.filter.trim().to_lowercase();
        self.visible = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                let query_match = query.is_empty()
                    || row.url.to_lowercase().contains(&query)
                    || row.method.to_lowercase().contains(&query)
                    || row.status.to_string().contains(&query);
                let protocol_match = self.protocol_filter.is_empty()
                    || match self.protocol_filter.as_str() {
                        "HTTPS" => row.url.starts_with("https:"),
                        "HTTP" => row.url.starts_with("http:"),
                        "HTTP1" => row.protocol.contains('1'),
                        "HTTP2" => row.protocol.contains('2'),
                        "HTTP3" => row.protocol.contains('3'),
                        _ => false,
                    };
                (query_match
                    && protocol_match
                    && (self.host_filter.is_empty() || row.host == self.host_filter)
                    && (self.app_filter.is_empty() || row.application == self.app_filter)
                    && (self.status_filter == 0 || row.status / 100 == self.status_filter)
                    && (self.type_filter.is_empty()
                        || row.content_type.contains(&self.type_filter)))
                .then_some(index)
            })
            .collect();
        let rows = &self.rows;
        let numbers = &self.sequence;
        let column = self.sort_column;
        self.visible.sort_by(|&a, &b| {
            let a = &rows[a];
            let b = &rows[b];
            let order = match column {
                2 => a.method.cmp(&b.method),
                3 => a.url.cmp(&b.url),
                4 => a.application.cmp(&b.application),
                5 => a.status.cmp(&b.status),
                6 => a.server_ip.cmp(&b.server_ip),
                7 => a.duration_ms.cmp(&b.duration_ms),
                8 => a.response_bytes.cmp(&b.response_bytes),
                _ => numbers.get(&a.id).cmp(&numbers.get(&b.id)),
            };
            if self.ascending {
                order
            } else {
                order.reverse()
            }
        });
    }
    fn receive(&mut self) {
        let inbox = std::mem::take(&mut *self.worker.inbox.lock().unwrap());
        if let Some(ready) = inbox.ready {
            match ready {
                Ok(path) => {
                    self.ca_path = path;
                    self.ready = true;
                }
                Err(error) => self.error = Some(error),
            }
        }
        if let Some(snapshot) = inbox.snapshot {
            self.running = snapshot.running;
            self.intercept_tls = snapshot.intercept_tls;
            if self.port != snapshot.port {
                self.port = snapshot.port;
                self.port_editor = self.port.to_string();
            }
            self.history_error = snapshot.history_error;
            if let Some(rows) = snapshot.rows {
                self.rows = rows;
                self.sequence
                    .retain(|id, _| self.rows.iter().any(|row| &row.id == id));
                for row in &self.rows {
                    if !self.sequence.contains_key(&row.id) {
                        self.sequence.insert(row.id.clone(), self.next_sequence);
                        self.next_sequence += 1;
                    }
                }
                self.refilter();
                if self
                    .selected
                    .as_ref()
                    .is_some_and(|id| !self.rows.iter().any(|row| &row.id == id))
                {
                    self.select(None);
                } else if self.messages.iter().any(|panel| {
                    panel
                        .viewer
                        .as_ref()
                        .is_some_and(|viewer| viewer.detail.notice.is_some())
                }) {
                    self.select(self.selected.clone());
                }
            }
        }
        for (index, result) in inbox.details.into_iter().enumerate() {
            if let Some((generation, result)) = result {
                if generation == self.generation {
                    match result {
                        Ok(mut detail) => {
                            let content = Content::new(std::mem::take(&mut detail.bytes));
                            let panel = &mut self.messages[index];
                            if matches!(content, Content::Binary(_)) {
                                panel.hex = true;
                            }
                            panel.viewer = Some(Viewer { detail, content });
                        }
                        Err(error) => self.error = Some(error),
                    }
                }
            }
        }
        if let Some(error) = inbox.error {
            self.error = Some(error);
        }
    }
}
impl eframe::App for Recho {
    fn logic(&mut self, _: &egui::Context, _: &mut eframe::Frame) {
        self.receive();
    }
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        workbench::show(self, ui);
    }
}
fn options() -> Result<(PathBuf, u16), String> {
    let directory = if let Some(directory) = std::env::var_os("RECHO_DATA_DIR") {
        PathBuf::from(directory)
    } else {
        let base = std::env::var_os(if cfg!(windows) {
            "LOCALAPPDATA"
        } else {
            "HOME"
        })
        .ok_or("Set RECHO_DATA_DIR to an absolute user data directory")?;
        PathBuf::from(base).join(if cfg!(target_os = "macos") {
            "Library/Application Support/recho"
        } else {
            "recho"
        })
    };
    if !directory.is_absolute() {
        return Err("RECHO_DATA_DIR must be absolute".into());
    }
    let port = std::env::var("RECHO_PROXY_PORT")
        .unwrap_or_else(|_| "0".into())
        .parse::<u16>()
        .map_err(|_| "RECHO_PROXY_PORT must be 0–65535")?;
    Ok((directory, port))
}
fn main() -> eframe::Result {
    let (directory, port) = options()
        .map_err(|error| eframe::Error::AppCreation(Box::new(std::io::Error::other(error))))?;
    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("recho")
            .with_inner_size([1450.0, 900.0])
            .with_min_inner_size([1050.0, 600.0])
            .with_app_id("io.github.jyy2luck.recho")
            .with_icon(egui::IconData::default()),
        renderer: eframe::Renderer::Glow,
        multisampling: 0,
        depth_buffer: 0,
        stencil_buffer: 0,
        ..Default::default()
    };
    eframe::run_native(
        "recho",
        native,
        Box::new(move |cc| Ok(Box::new(Recho::new(cc, directory, port)))),
    )
}
