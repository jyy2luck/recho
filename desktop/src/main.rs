#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod application_icons;
mod body;
mod capture_filter_ui;
mod capture_spaces;
mod certificate_ui;
mod composer;
mod files;
mod library_ui;
mod mcp_ui;
#[cfg(target_os = "macos")]
mod native_menu;
mod network_ui;
mod platform;
mod rules_ui;
mod search_presets;
mod sharing;
mod theme;
#[cfg(windows)]
mod title_bar;
mod workbench;
mod worker;

use body::{Content, hex_row};
use eframe::egui::{self, RichText};
use recho_core::native::{
    ConditionSet, Detail, HistoryDay, HistoryDeletePlan, HistoryDeletion, Phase, QuickFilters, Row,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::Ipv4Addr,
    path::PathBuf,
};
use worker::{Command, DetailHandle, HistoryHandle, RequestHandle, Worker};

const SEMIBOLD_FAMILY: &str = "semibold";
const TABULAR_FAMILY: &str = "tabular";

fn semibold_font(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name(SEMIBOLD_FAMILY.into()))
}

fn with_faster_vertical_scroll(ui: &mut egui::Ui, show: impl FnOnce(&mut egui::Ui)) {
    const WHEEL_MULTIPLIER: f32 = 3.0;
    ui.input_mut(|input| input.smooth_scroll_delta.y *= WHEEL_MULTIPLIER);
    show(ui);
    // Pass any unconsumed wheel input on at its original speed.
    ui.input_mut(|input| input.smooth_scroll_delta.y /= WHEEL_MULTIPLIER);
}

#[cfg(test)]
#[test]
fn faster_vertical_scroll_keeps_horizontal_speed_and_passes_unused_input() {
    let ctx = egui::Context::default();
    for (delta, expected_offset, remaining) in [
        (
            egui::vec2(0.0, 40.0),
            egui::Vec2::ZERO,
            egui::vec2(0.0, 40.0),
        ),
        (
            egui::vec2(-40.0, -40.0),
            egui::vec2(40.0, 120.0),
            egui::Vec2::ZERO,
        ),
    ] {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(320.0, 240.0),
                )),
                events: vec![egui::Event::PointerMoved(egui::pos2(20.0, 20.0))],
                ..Default::default()
            },
            |ui| {
                ui.input_mut(|input| input.smooth_scroll_delta = delta);
                with_faster_vertical_scroll(ui, |ui| {
                    let output = egui::ScrollArea::both().show(ui, |ui| {
                        ui.allocate_space(egui::vec2(800.0, 800.0));
                    });
                    assert_eq!(output.state.offset, expected_offset);
                });
                assert_eq!(ui.input(|input| input.smooth_scroll_delta()), remaining);
            },
        );
        output.textures_delta.clear();
    }
}

fn desktop_fonts() -> egui::FontDefinitions {
    use egui::FontFamily::{Monospace, Name, Proportional};

    let mut fonts = egui::FontDefinitions::default();
    #[cfg(windows)]
    let windows_fonts =
        std::env::var_os("SystemRoot").map(|root| PathBuf::from(root).join("Fonts"));
    #[cfg(windows)]
    if let Some(directory) = &windows_fonts {
        prefer_windows_fonts(&mut fonts, directory);
    }
    for (name, bytes) in [
        (
            "geist",
            include_bytes!("../assets/fonts/Geist-Regular.ttf").as_slice(),
        ),
        (
            "geist-semibold",
            include_bytes!("../assets/fonts/Geist-SemiBold.ttf").as_slice(),
        ),
        (
            "geist-tabular-digits",
            include_bytes!("../assets/fonts/RechoGeistTabular-Regular.ttf").as_slice(),
        ),
        (
            "cjk",
            include_bytes!("../assets/fonts/DroidSansFallback.ttf").as_slice(),
        ),
        (
            "icons",
            include_bytes!("../assets/fonts/MaterialIcons-Regular.ttf").as_slice(),
        ),
    ] {
        fonts.font_data.insert(
            name.into(),
            std::sync::Arc::new(egui::FontData::from_static(bytes)),
        );
    }
    for family in [Proportional, Monospace] {
        fonts.families.entry(family).or_default().push("cjk".into());
    }
    let proportional = fonts.families.entry(Proportional).or_default();
    proportional.insert(0, "geist".into());
    let mut semibold = proportional.clone();
    semibold[0] = "geist-semibold".into();
    let mut tabular = proportional.clone();
    tabular.insert(0, "geist-tabular-digits".into());
    #[cfg(windows)]
    if let Some(bytes) = windows_fonts.and_then(|dir| std::fs::read(dir.join("msyhbd.ttc")).ok()) {
        fonts.font_data.insert(
            "msyh-bold".into(),
            std::sync::Arc::new(egui::FontData::from_owned(bytes)),
        );
        semibold.insert(1, "msyh-bold".into());
    }
    fonts
        .families
        .insert(Name(SEMIBOLD_FAMILY.into()), semibold);
    fonts.families.insert(Name(TABULAR_FAMILY.into()), tabular);
    fonts
        .families
        .insert(Name("icons".into()), vec!["icons".into()]);
    fonts
}

#[test]
fn capture_table_font_uses_tabular_digits_with_text_fallback() {
    let ctx = egui::Context::default();
    ctx.set_fonts(desktop_fonts());
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        let regular = egui::FontId::proportional(13.0);
        let tabular = egui::FontId::new(13.0, egui::FontFamily::Name(TABULAR_FAMILY.into()));
        ui.fonts_mut(|fonts| {
            let width = fonts.glyph_width(&tabular, '0');
            assert!(width > 0.0);
            for digit in '1'..='9' {
                assert_eq!(fonts.glyph_width(&tabular, digit), width);
            }
            assert_ne!(
                fonts.glyph_width(&regular, '0'),
                fonts.glyph_width(&regular, '1')
            );
            for text in ['A', '请'] {
                assert!(fonts.has_glyph(&tabular, text));
                assert_eq!(
                    fonts.glyph_width(&tabular, text),
                    fonts.glyph_width(&regular, text)
                );
            }
        });
    });
    output.textures_delta.clear();
}

#[cfg(windows)]
fn prefer_windows_fonts(fonts: &mut egui::FontDefinitions, directory: &std::path::Path) {
    use egui::FontFamily::{Monospace, Proportional};

    if let Ok(bytes) = std::fs::read(directory.join("msyh.ttc")) {
        // Face 0 of the collection is Microsoft YaHei Regular.
        fonts.font_data.insert(
            "msyh".into(),
            std::sync::Arc::new(egui::FontData::from_owned(bytes)),
        );
        fonts
            .families
            .entry(Proportional)
            .or_default()
            .insert(0, "msyh".into());
        fonts
            .families
            .entry(Monospace)
            .or_default()
            .push("msyh".into());
    }
    if let Ok(bytes) = std::fs::read(directory.join("segoeui.ttf")) {
        fonts.font_data.insert(
            "segoe-ui".into(),
            std::sync::Arc::new(egui::FontData::from_owned(bytes)),
        );
        fonts
            .families
            .entry(Proportional)
            .or_default()
            .insert(0, "segoe-ui".into());
    }
}

#[cfg(all(test, windows))]
#[test]
fn windows_fonts_preserve_builtin_fallbacks() {
    let directory = tempfile::tempdir().unwrap();
    let defaults = egui::FontDefinitions::default();
    let mut fonts = defaults.clone();
    prefer_windows_fonts(&mut fonts, directory.path());
    assert_eq!(fonts.families, defaults.families);

    let fixture = include_bytes!("../assets/fonts/DroidSansFallback.ttf");
    for name in ["segoeui.ttf", "msyh.ttc"] {
        std::fs::write(directory.path().join(name), fixture).unwrap();
    }
    prefer_windows_fonts(&mut fonts, directory.path());
    let proportional = &fonts.families[&egui::FontFamily::Proportional];
    assert_eq!(&proportional[..2], ["segoe-ui", "msyh"]);
    assert_eq!(
        &proportional[2..],
        defaults.families[&egui::FontFamily::Proportional]
    );
    let monospace = &fonts.families[&egui::FontFamily::Monospace];
    assert_eq!(
        &monospace[..monospace.len() - 1],
        defaults.families[&egui::FontFamily::Monospace]
    );
    assert_eq!(monospace.last().unwrap(), "msyh");
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Navigation {
    Capture,
    Collections,
    Environments,
    History,
    Tools,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum HistoryScope {
    All,
    Day(String),
}

impl HistoryScope {
    fn date(&self) -> Option<&str> {
        match self {
            Self::All => None,
            Self::Day(date) => Some(date),
        }
    }
}

#[derive(Default)]
struct HistoryTab {
    rows: Vec<Row>,
    offset: usize,
    load: Option<HistoryHandle>,
    loaded: bool,
    has_more: bool,
    quick: QuickFilters,
    conditions: ConditionSet,
    filter_draft: ConditionSet,
    filter_error: Option<String>,
    preset: Option<String>,
}

struct Viewer {
    detail: Detail,
    content: Content,
    structured: Option<Result<serde_json::Value, String>>,
    response: body::response::View,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum BodyFormat {
    Raw,
    Hex,
    Json,
    Tree,
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
    format: Option<BodyFormat>,
    viewer: Option<Viewer>,
}
impl MessagePanel {
    fn new(tab: MessageTab) -> Self {
        Self {
            tab,
            format: None,
            viewer: None,
        }
    }
}

struct Recho {
    #[cfg(windows)]
    title_bar: title_bar::TitleBar,
    worker: Worker,
    theme: theme::Theme,
    theme_path: PathBuf,
    composer: composer::Composer,
    rules: rules_ui::RulesUi,
    library: library_ui::LibraryUi,
    network: network_ui::NetworkUi,
    mcp: mcp_ui::McpUi,
    workspace_ops: Vec<Value>,
    active_workspace_op: Option<(String, Result<Value, String>, Option<RequestHandle>)>,
    spaces: capture_spaces::Spaces,
    search_presets: search_presets::SearchPresets,
    capture_filters: Vec<String>,
    highlight_editor: Option<(recho_core::native::ConditionSet, String)>,
    capture_positions: BTreeMap<String, usize>,
    har_file: Option<(files::FilePick, Option<Vec<String>>)>,
    response_save: Option<body::response::SaveHandle>,
    notice: Option<String>,
    certificates: certificate_ui::CertificateUi,
    system_proxy: (bool, bool),
    proxy_preferences: platform::ProxyPreferences,
    proxy_preferences_saving: bool,
    confirm_proxy_auto: bool,
    navigation: Navigation,
    history_tabs: BTreeMap<HistoryScope, HistoryTab>,
    history_scope: Option<HistoryScope>,
    history_days: Vec<HistoryDay>,
    history_selected_dates: BTreeSet<String>,
    history_date_anchor: Option<String>,
    history_delete: Option<HistoryDeletePlan>,
    history_delete_pending: bool,
    curl_copy: Option<(Row, DetailHandle)>,
    closing: bool,
    closing_proxy: Option<tokio::sync::oneshot::Receiver<Result<(), String>>>,
    discard_on_close: bool,
    ready: bool,
    ca_path: PathBuf,
    running: bool,
    proxy_host: Ipv4Addr,
    port: u16,
    port_dialog: Option<network_ui::PortDialog>,
    rows: Vec<Row>,
    visible: Vec<usize>,
    application_icons: application_icons::ApplicationIcons,
    host_filter: String,
    app_filter: String,
    quick_filters: QuickFilters,
    sort_column: usize,
    ascending: bool,
    selected: Option<String>,
    selection: BTreeSet<String>,
    selection_anchor: Option<String>,
    generation: u64,
    messages: [MessagePanel; 2],
    details_open: bool,
    sidebar_visible: bool,
    connection_open: bool,
    #[cfg(not(target_os = "macos"))]
    about_open: bool,
    error: Option<String>,
    history_error: Option<String>,
}
impl Recho {
    fn new(
        cc: &eframe::CreationContext<'_>,
        directory: PathBuf,
        port: Option<u16>,
        #[cfg(windows)] title_bar: title_bar::TitleBar,
    ) -> Self {
        let theme_path = directory.join("desktop-theme.json");
        let (theme, theme_error) = match theme::Theme::load(&theme_path) {
            Ok(theme) => (theme, None),
            Err(error) => (theme::Theme::default(), Some(error)),
        };
        theme.apply(&cc.egui_ctx);
        cc.egui_ctx.set_fonts(desktop_fonts());
        Self {
            #[cfg(windows)]
            title_bar,
            theme,
            theme_path,
            spaces: capture_spaces::Spaces::new(directory.join("desktop-captures.json")),
            search_presets: search_presets::SearchPresets::new(
                directory.join("desktop-search-presets.json"),
            ),
            worker: Worker::start(directory, port, cc.egui_ctx.clone()),
            capture_positions: BTreeMap::new(),
            capture_filters: Vec::new(),
            highlight_editor: None,
            har_file: None,
            response_save: None,
            notice: None,
            network: network_ui::NetworkUi::default(),
            mcp: mcp_ui::McpUi::default(),
            workspace_ops: Vec::new(),
            active_workspace_op: None,
            composer: composer::Composer::default(),
            rules: rules_ui::RulesUi::default(),
            library: library_ui::LibraryUi::default(),
            certificates: certificate_ui::CertificateUi::default(),
            system_proxy: (false, false),
            proxy_preferences: platform::ProxyPreferences::default(),
            proxy_preferences_saving: false,
            confirm_proxy_auto: false,
            navigation: Navigation::Capture,
            history_tabs: BTreeMap::new(),
            history_scope: None,
            history_days: Vec::new(),
            history_selected_dates: BTreeSet::new(),
            history_date_anchor: None,
            history_delete: None,
            history_delete_pending: false,
            curl_copy: None,
            closing: false,
            closing_proxy: None,
            discard_on_close: false,
            ready: false,
            ca_path: PathBuf::new(),
            running: false,
            proxy_host: Ipv4Addr::LOCALHOST,
            port: 0,
            port_dialog: None,
            rows: Vec::new(),
            visible: Vec::new(),
            application_icons: application_icons::ApplicationIcons::default(),
            host_filter: String::new(),
            app_filter: String::new(),
            quick_filters: QuickFilters::default(),
            sort_column: 0,
            ascending: true,
            selected: None,
            selection: BTreeSet::new(),
            selection_anchor: None,
            generation: 0,
            messages: [
                MessagePanel::new(MessageTab::Overview),
                MessagePanel::new(MessageTab::Body),
            ],
            details_open: false,
            sidebar_visible: true,
            connection_open: false,
            #[cfg(not(target_os = "macos"))]
            about_open: false,
            error: theme_error,
            history_error: None,
        }
    }

    fn set_theme(&mut self, next: theme::Theme, ctx: &egui::Context) {
        self.theme = next;
        next.apply(ctx);
        if let Err(error) = next.save(&self.theme_path) {
            self.error = Some(error);
        }
    }

    fn send(&mut self, command: Command) {
        if let Err(error) = self.worker.send(command) {
            self.error = Some(error);
        }
    }

    fn proxy_address(&self) -> String {
        format!("{}:{}", self.proxy_host, self.port)
    }

    fn set_proxy_preferences(&mut self, preferences: platform::ProxyPreferences) {
        if self.proxy_preferences_saving {
            return;
        }
        match self
            .worker
            .send(Command::SystemProxyPreferences(preferences))
        {
            Ok(()) => self.proxy_preferences_saving = true,
            Err(error) => self.error = Some(error),
        }
    }
    fn toggle_proxy_auto(&mut self) {
        if self.proxy_preferences.auto {
            self.set_proxy_preferences(platform::ProxyPreferences {
                auto: false,
                ..self.proxy_preferences
            });
        } else {
            self.confirm_proxy_auto = true;
        }
    }
    fn toggle_capture(&mut self) {
        self.send(Command::Configure {
            running: Some(!self.running),
            intercept_tls: None,
        });
    }
    fn select(&mut self, id: Option<String>) {
        self.selection = id.iter().cloned().collect();
        self.selection_anchor = id.clone();
        self.select_detail(id);
    }
    fn select_detail(&mut self, id: Option<String>) {
        if self.selected != id {
            self.messages[1].format = None;
        }
        self.selected = id;
        self.generation += 1;
        for message in &mut self.messages {
            message.viewer = None;
        }
        if self.details_open {
            self.load_details();
        }
    }
    fn select_row(&mut self, id: String, modifiers: egui::Modifiers) {
        let order = if modifiers.shift {
            workbench::visible_rows(
                &self.rows,
                &self.visible,
                self.history_scope
                    .as_ref()
                    .and_then(|scope| self.history_tabs.get(scope))
                    .map(|tab| tab.rows.as_slice()),
            )
            .into_iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        select_list_item(
            &mut self.selection,
            &mut self.selection_anchor,
            &id,
            modifiers,
            &order,
        );
        let detail = if self.selection.contains(&id) {
            Some(id)
        } else {
            self.selection.iter().next().cloned()
        };
        self.select_detail(detail);
    }
    fn active_rows(&self) -> &[Row] {
        self.active_history().map_or(&self.rows, |tab| &tab.rows)
    }
    fn navigate(&mut self, next: Navigation) {
        self.library.close();
        self.navigation = next;
        self.sidebar_visible = true;
        if next == Navigation::History {
            self.open_history(self.history_scope.clone().unwrap_or(HistoryScope::All));
        } else {
            self.leave_history();
        }
    }
    fn active_history(&self) -> Option<&HistoryTab> {
        self.history_tabs.get(self.history_scope.as_ref()?)
    }
    fn active_history_mut(&mut self) -> Option<&mut HistoryTab> {
        self.history_tabs.get_mut(self.history_scope.as_ref()?)
    }
    fn open_history(&mut self, scope: HistoryScope) {
        self.library.close();
        self.navigation = Navigation::History;
        self.composer.show_capture();
        if self.history_scope.as_ref() == Some(&scope) {
            return;
        }
        self.remember_capture();
        if let Some(tab) = self.active_history_mut() {
            tab.load = None;
        }
        self.select(None);
        self.history_scope = Some(scope.clone());
        let offset = self.history_tabs.entry(scope).or_default().offset;
        if self.ready {
            self.load_history(offset);
        }
    }
    fn leave_history(&mut self) {
        let Some(scope) = self.history_scope.take() else {
            return;
        };
        if let Some(tab) = self.history_tabs.get_mut(&scope) {
            tab.load = None;
        }
        self.select(None);
        if let Some(space) = self.spaces.current() {
            self.selection = space.selection.clone();
            self.details_open = space.details_open;
            self.select_detail(space.detail.clone());
            self.rebuild_capture_rows();
        }
    }
    fn close_history(&mut self, scope: &HistoryScope) {
        if self.history_scope.as_ref() == Some(scope) {
            self.leave_history();
        }
        self.history_tabs.remove(scope);
    }
    fn set_incognito(&mut self, enabled: bool) {
        match self.worker.request(
            "PATCH",
            "/settings",
            serde_json::json!({
                "incognito": enabled, "_expected": {"incognito": self.spaces.incognito}
            }),
        ) {
            Ok(reply) => self.spaces.settings = Some(reply),
            Err(error) => self.error = Some(error),
        }
    }
    fn open_network(&mut self, section: network_ui::NetworkSection) {
        self.network.open(section);
    }
    fn load_history(&mut self, offset: usize) {
        let Some(scope) = self.history_scope.as_ref() else {
            return;
        };
        let tab = self.history_tabs.get_mut(scope).unwrap();
        tab.load = None;
        tab.rows.clear();
        tab.loaded = false;
        tab.has_more = false;
        let date = scope.date().map(str::to_owned);
        match self
            .worker
            .history(offset, date, tab.conditions.clone(), tab.quick.clone())
        {
            Ok(request) => {
                tab.offset = offset;
                tab.load = Some(request);
            }
            Err(error) => self.error = Some(error),
        }
    }

    fn prepare_history_delete(&mut self, target: HistoryDeletion) {
        if !self.history_delete_pending && self.history_delete.is_none() {
            match self.worker.send(Command::PrepareHistoryDelete(target)) {
                Ok(()) => self.history_delete_pending = true,
                Err(error) => self.error = Some(error),
            }
        }
    }

    fn confirm_history_delete(&mut self, plan: HistoryDeletePlan) {
        match self.worker.send(Command::DeleteHistory(plan)) {
            Ok(()) => self.history_delete_pending = true,
            Err(error) => self.error = Some(error),
        }
    }
    fn choose_har(&mut self, ctx: &egui::Context, export: bool) {
        if self.har_file.is_some() {
            return;
        }
        let selection = export.then(|| {
            self.active_rows()
                .iter()
                .filter(|row| self.selection.contains(&row.id))
                .map(|row| row.id.clone())
                .collect::<Vec<_>>()
        });
        if selection.as_ref().is_some_and(Vec::is_empty) {
            self.error = Some("请先选择要导出的记录".into());
            return;
        }
        self.har_file = Some((files::pick("har", export, ctx), selection));
    }
    fn poll_har(&mut self) {
        if let Some((reply, ids)) = &mut self.har_file {
            let result = match reply.try_recv() {
                Ok(result) => result,
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return,
                Err(error) => Err(error.to_string()),
            };
            match result {
                Ok(Some(path)) => {
                    let command = match ids.take() {
                        Some(ids) => Command::ExportHar(path, ids),
                        None => Command::ImportHar(path),
                    };
                    self.send(command);
                }
                Ok(None) => {}
                Err(error) => self.error = Some(error),
            }
            self.har_file = None;
        }
    }
    fn poll_response_save(&mut self) {
        let Some(request) = &mut self.response_save else {
            return;
        };
        let result = match request.try_recv() {
            Ok(result) => result,
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return,
            Err(error) => Err(error.to_string()),
        };
        self.response_save = None;
        match result {
            Ok(true) => self.notice = Some("响应正文已保存".into()),
            Ok(false) => {}
            Err(error) => self.error = Some(format!("保存响应正文失败：{error}")),
        }
    }
    fn edit_capture(&mut self, id: &str) {
        if let Some(row) = self.active_rows().iter().find(|row| row.id == id).cloned() {
            let viewer = self.messages[0]
                .viewer
                .as_ref()
                .filter(|viewer| viewer.detail.id == id);
            self.composer.open_capture(
                &self.worker,
                &row,
                viewer.map(|viewer| &viewer.detail),
                viewer.map(|viewer| viewer.content.bytes()),
            );
        }
    }
    fn copy_curl(&mut self, id: String) {
        if let Some(row) = self.active_rows().iter().find(|row| row.id == id) {
            let row = row.clone();
            match self.worker.detail(id, Phase::Request) {
                Ok(reply) => self.curl_copy = Some((row, reply)),
                Err(error) => self.error = Some(error),
            }
        }
    }
    fn poll_curl(&mut self, ctx: &egui::Context) {
        if let Some((row, reply)) = self.curl_copy.as_mut() {
            let result = match reply.try_recv() {
                Ok(Ok(detail)) => composer::capture_curl(row, &detail, &detail.bytes),
                Ok(Err(error)) => Err(error),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return,
                Err(error) => Err(error.to_string()),
            };
            match result {
                Ok(curl) => ctx.copy_text(curl),
                Err(error) => self.error = Some(error),
            }
            self.curl_copy = None;
        }
    }
    fn poll_workspace_operations(&mut self) {
        if self.active_workspace_op.is_none() {
            let Some(operation) = self.workspace_ops.first() else {
                return;
            };
            let Some(id) = operation["id"].as_str() else {
                return;
            };
            if !self.composer.flush_pending(&self.worker) && self.composer.save_error().is_none() {
                return;
            }
            let result = match self.composer.save_error() {
                Some(error) => Err(error.into()),
                None => self.composer.workspace_operation_once(
                    id,
                    operation["name"].as_str().unwrap_or_default(),
                    &operation["arguments"],
                ),
            };
            self.active_workspace_op = Some((id.to_owned(), result, None));
        }
        let Some((id, result, reply)) = &mut self.active_workspace_op else {
            return;
        };
        if let Some(handle) = reply {
            if matches!(
                handle.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ) {
                return;
            }
            self.workspace_ops
                .retain(|operation| operation["id"] != id.as_str());
            self.active_workspace_op = None;
            return;
        }
        if !self
            .workspace_ops
            .iter()
            .any(|operation| operation["id"] == id.as_str())
        {
            self.active_workspace_op = None;
            return;
        }
        let result = if let Err(error) = result {
            json!({"error": error})
        } else if self.composer.flush_pending(&self.worker) {
            json!({"result": result.as_ref().ok()})
        } else if let Some(error) = self.composer.save_error() {
            json!({"error": error})
        } else {
            return;
        };
        match self
            .worker
            .request("POST", format!("/ui-operations/{id}"), result)
        {
            Ok(handle) => *reply = Some(handle),
            Err(_) => {}
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
        self.visible = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                let mut applicable = self
                    .capture_filters
                    .iter()
                    .filter_map(|id| row.list_matches.get(id))
                    .peekable();
                let automation_match =
                    applicable.peek().is_none() || applicable.any(|value| *value);
                (automation_match
                    && self
                        .spaces
                        .matches
                        .as_ref()
                        .is_none_or(|ids| ids.contains(&row.id))
                    && self.quick_filters.matches(row)
                    && (self.host_filter.is_empty() || row.host == self.host_filter)
                    && (self.app_filter.is_empty() || row.application == self.app_filter))
                    .then_some(index)
            })
            .collect();
        let rows = &self.rows;
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
                _ => a.live_id.cmp(&b.live_id),
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
                    if let Some(offset) = self.active_history().map(|tab| tab.offset) {
                        self.load_history(offset);
                    }
                }
                Err(error) => self.error = Some(error),
            }
        }
        if let Some(host) = inbox.proxy_host {
            self.proxy_host = host;
        }
        if let Some(snapshot) = inbox.snapshot {
            if snapshot.running && !self.running && !snapshot.intercept_tls_configured {
                self.certificates.open_capture_setup();
            }
            self.running = snapshot.running;
            self.port = snapshot.port;
            self.history_error = snapshot.history_error;
            self.spaces.incognito = snapshot.incognito;
            self.open_capture_space(snapshot.group);
            if self.capture_filters != snapshot.enabled_list_filters {
                self.capture_filters = snapshot.enabled_list_filters;
                self.refilter();
            }
        }
        if let Some(captures) = inbox.captures
            && captures.group == self.spaces.group
        {
            if captures.replace {
                self.capture_positions.clear();
                self.rows.clear();
            }
            let mut incoming: Vec<_> = captures.rows.into_values().collect();
            incoming.sort_by(|a, b| a.started_at.cmp(&b.started_at));
            for row in incoming {
                match self.capture_positions.entry(row.id.clone()) {
                    std::collections::btree_map::Entry::Occupied(position) => {
                        self.rows[*position.get()] = row;
                    }
                    std::collections::btree_map::Entry::Vacant(position) => {
                        position.insert(self.rows.len());
                        self.rows.push(row);
                    }
                }
            }
            self.rebuild_capture_rows();
            if self.messages.iter().any(|panel| {
                panel
                    .viewer
                    .as_ref()
                    .is_some_and(|viewer| viewer.detail.notice.is_some())
            }) {
                self.select_detail(self.selected.clone());
            }
        }
        for group in inbox.closed_captures {
            if group != self.spaces.group {
                self.spaces.items.remove(&group);
            }
        }
        for (index, result) in inbox.details.into_iter().enumerate() {
            if let Some((generation, result)) = result {
                if generation == self.generation {
                    match result {
                        Ok(mut detail) => {
                            let content = Content::new(std::mem::take(&mut detail.bytes));
                            let panel = &mut self.messages[index];
                            if detail.phase == Phase::Request
                                && matches!(content, Content::Binary(_))
                            {
                                panel.format = Some(BodyFormat::Hex);
                            }
                            panel.viewer = Some(Viewer {
                                detail,
                                content,
                                structured: None,
                                response: body::response::View::default(),
                            });
                        }
                        Err(error) => self.error = Some(error),
                    }
                }
            }
        }
        if let Some(error) = inbox.error {
            self.error = Some(error);
        }
        if let Some(pending) = inbox.breakpoints {
            self.rules.update_pending(pending);
        }
        if let Some(mut pending) = inbox.workspace_operations {
            self.workspace_ops = pending["operations"]
                .take()
                .as_array()
                .cloned()
                .unwrap_or_default();
        }
        if let Some(status) = inbox.system_proxy {
            self.system_proxy = status;
        }
        if let Some(preferences) = inbox.proxy_preferences {
            self.proxy_preferences = preferences;
            self.proxy_preferences_saving = false;
        }
        if let Some(notice) = inbox.notice {
            self.notice = Some(notice);
        }
        if let Some((group, name, count)) = inbox.imported {
            self.spaces.items.insert(
                group.clone(),
                capture_spaces::Space {
                    name,
                    ..Default::default()
                },
            );
            self.navigate(Navigation::Capture);
            self.composer.show_capture();
            self.activate_capture(group);
            self.notice = Some(format!("已导入 {count} 条 HAR 记录"));
        }
        if let Some(result) = inbox.history_delete_plan {
            self.history_delete_pending = false;
            match result {
                Ok(plan) if self.navigation == Navigation::History => {
                    self.history_delete = Some(plan)
                }
                Ok(_) => {}
                Err(error) => self.error = Some(error),
            }
        }
        if inbox.history_delete_finished {
            self.history_delete_pending = false;
        }
        if inbox.history_changed && self.history_scope.is_some() {
            self.load_history(0);
        }
        if let Some(request) = self.active_history_mut().and_then(|tab| tab.load.as_mut()) {
            match request.try_recv() {
                Ok(Ok(page)) => {
                    if self
                        .selected
                        .as_ref()
                        .is_some_and(|id| !page.rows.iter().any(|row| &row.id == id))
                    {
                        self.select(None);
                    }
                    self.selection
                        .retain(|id| page.rows.iter().any(|row| &row.id == id));
                    let tab = self.active_history_mut().unwrap();
                    tab.rows = page.rows;
                    tab.has_more = page.has_more;
                    tab.loaded = true;
                    tab.load = None;
                    self.history_days = page.days;
                    self.history_selected_dates
                        .retain(|date| self.history_days.iter().any(|day| &day.date == date));
                    if self
                        .history_scope
                        .as_ref()
                        .and_then(HistoryScope::date)
                        .is_some_and(|date| !self.history_days.iter().any(|day| &day.date == date))
                    {
                        self.open_history(HistoryScope::All);
                    }
                    self.history_tabs.retain(|scope, _| {
                        scope
                            .date()
                            .is_none_or(|date| self.history_days.iter().any(|day| day.date == date))
                    });
                }
                Ok(Err(error)) => {
                    self.error = Some(error);
                    self.active_history_mut().unwrap().load = None;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
                Err(error) => {
                    self.error = Some(error.to_string());
                    self.active_history_mut().unwrap().load = None;
                }
            }
        }
    }
}
fn select_list_item(
    selection: &mut BTreeSet<String>,
    anchor: &mut Option<String>,
    id: &str,
    modifiers: egui::Modifiers,
    order: &[&str],
) {
    let additive = modifiers.command || modifiers.ctrl;
    if modifiers.shift {
        let position = |id: &str| order.iter().position(|&item| item == id);
        if let (Some(start), Some(end)) = (anchor.as_deref().and_then(position), position(id)) {
            if !additive {
                selection.clear();
            }
            selection.extend(
                order[start.min(end)..=start.max(end)]
                    .iter()
                    .map(|&id| id.to_owned()),
            );
            return;
        }
    }
    *anchor = Some(id.into());
    if !additive {
        selection.clear();
    }
    if !additive || modifiers.shift || !selection.remove(id) {
        selection.insert(id.into());
    }
}

impl eframe::App for Recho {
    fn logic(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.receive();
        self.certificates.poll(ctx, &self.ca_path);
        if let Some(result) = self.certificates.take_background_feedback() {
            match result {
                Ok(message) => self.notice = Some(message),
                Err(error) => self.error = Some(error),
            }
        }
        self.rules.poll();
        self.library.poll(&self.worker);
        self.network.poll();
        self.mcp.poll();
        self.poll_capture_spaces();
        self.remember_capture();
        self.poll_curl(ctx);
        self.poll_har();
        self.poll_response_save();
        self.composer.poll(&self.worker, self.ready, ctx);
        self.poll_workspace_operations();
        let close = ctx.input(|input| input.viewport().close_requested());
        self.spaces.persist(ctx, close);
        if close && self.system_proxy.1 {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if self.closing_proxy.is_none() {
                match self.worker.restore_system_proxy() {
                    Ok(request) => self.closing_proxy = Some(request),
                    Err(error) => self.error = Some(error),
                }
            }
            return;
        }
        if let Some(request) = &mut self.closing_proxy {
            let result = match request.try_recv() {
                Ok(result) => result,
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return,
                Err(error) => Err(error.to_string()),
            };
            self.closing_proxy = None;
            match result {
                Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                Err(error) => {
                    self.error = Some(format!(
                        "退出前恢复系统代理失败。请在连接设置中恢复后重试。{error}"
                    ));
                    self.connection_open = true;
                }
            }
            return;
        }
        if close && !self.discard_on_close && !self.composer.flush_pending(&self.worker) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.closing = true;
        } else if self.closing && self.composer.flush_pending(&self.worker) {
            self.closing = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        workbench::show(self, ui);
        self.search_presets.dialogs(ui.ctx());
        if self.closing {
            let message = self.composer.save_error().map(str::to_owned);
            if let Some(message) = message {
                theme::Theme::compact_window(ui.ctx(), "工作区保存失败")
                    .collapsible(false)
                    .show(ui.ctx(), |ui| {
                        ui.label(message);
                        if ui.button("返回工作区").clicked() {
                            self.closing = false;
                        }
                        if ui.button("退出并放弃未保存的更改").clicked() {
                            self.closing = false;
                            self.discard_on_close = true;
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
            }
        }
    }
}
fn options() -> Result<(PathBuf, Option<u16>), String> {
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
        .ok()
        .map(|value| {
            value
                .parse::<u16>()
                .map_err(|_| "RECHO_PROXY_PORT must be 0–65535".to_string())
        })
        .transpose()?;
    Ok((directory, port))
}
#[cfg(windows)]
fn windows_wgpu_options() -> eframe::WgpuConfiguration {
    let mut setup = eframe::egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.instance_descriptor.backends = wgpu::Backends::DX12;
    let default_descriptor = std::sync::Arc::clone(&setup.device_descriptor);
    setup.device_descriptor = std::sync::Arc::new(move |adapter| {
        let mut descriptor = default_descriptor(adapter);
        descriptor.memory_hints = wgpu::MemoryHints::MemoryUsage;
        // DX12 sizes its descriptor heap up front. Egui needs far fewer than the default million.
        descriptor.required_limits.max_non_sampler_bindings = 65_536;
        descriptor
    });
    eframe::WgpuConfiguration {
        // Prefer the latest frame when the window size changes faster than the display refreshes.
        surface: eframe::egui_wgpu::SurfaceConfig {
            present_mode: wgpu::PresentMode::Mailbox,
            ..eframe::egui_wgpu::SurfaceConfig::HIGH_THROUGHPUT
        },
        wgpu_setup: setup.into(),
        ..Default::default()
    }
}

fn main() -> eframe::Result {
    if std::env::args().nth(1).as_deref() == Some("--mcp") {
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())
            .and_then(|runtime| runtime.block_on(recho_core::run(true)));
        if let Err(error) = result {
            eprintln!("MCP: {error}");
            std::process::exit(1);
        }
        return Ok(());
    }
    let (directory, port) = options()
        .map_err(|error| eframe::Error::AppCreation(Box::new(std::io::Error::other(error))))?;
    #[cfg(target_os = "windows")]
    let icon = std::sync::Arc::new(
        eframe::icon_data::from_png_bytes(include_bytes!("../assets/icons/recho.png"))
            .expect("embedded Recho icon must be a valid PNG"),
    );
    #[cfg(not(target_os = "windows"))]
    let icon = std::sync::Arc::new(egui::IconData::default());

    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("recho")
            .with_inner_size([1450.0, 900.0])
            .with_min_inner_size([1050.0, 600.0])
            .with_app_id("io.github.jyy2luck.recho")
            .with_decorations(!cfg!(windows))
            .with_icon(icon.clone()),
        #[cfg(windows)]
        renderer: eframe::Renderer::Wgpu,
        #[cfg(windows)]
        wgpu_options: windows_wgpu_options(),
        #[cfg(not(windows))]
        renderer: eframe::Renderer::Glow,
        multisampling: 0,
        depth_buffer: 0,
        stencil_buffer: 0,
        ..Default::default()
    };
    eframe::run_native(
        "recho",
        native,
        Box::new(move |cc| {
            #[cfg(windows)]
            let title_bar = title_bar::TitleBar::attach(cc, &icon)?;
            Ok(Box::new(Recho::new(
                cc,
                directory,
                port,
                #[cfg(windows)]
                title_bar,
            )))
        }),
    )
}
