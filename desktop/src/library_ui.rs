use crate::body::TextRows;
use crate::rules_ui::{finished, label};
use crate::worker::{RequestHandle, Worker};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use eframe::egui;
use recho_core::native::Row;
use serde_json::{Value, json};
use std::{collections::BTreeSet, io::Read};

const ARCHIVE: &str = "/library/archive";
const DIFF_LIMIT: usize = 1024 * 1024;
const EXPORT_LIMIT: u64 = 64 * 1024 * 1024;

#[derive(Default)]
pub(super) struct LibraryUi {
    open: bool,
    compare: bool,
    folder: String,
    selected: String,
    archive: Option<Value>,
    bookmarks: Vec<(String, String)>,
    comparison_count: usize,
    reply: Option<(Reply, RequestHandle)>,
    queued: Option<(Vec<String>, bool, bool)>,
    edit: Option<Edit>,
    file: Option<(bool, crate::files::FilePick)>,
    error: Option<String>,
    diff: Option<Preview>,
    preview: Option<Preview>,
}

enum Reply {
    Load,
    Save,
    Snapshots { compare: bool, compare_now: bool },
}

enum Edit {
    Bookmark {
        id: String,
        name: String,
        folder: String,
    },
    AddFolder(String),
    RemoveFolder(String),
    Remove {
        id: String,
        compare: bool,
    },
}

struct Preview {
    body: String,
    rows: TextRows,
    partial: bool,
}

impl Preview {
    fn new(body: String, partial: bool) -> Self {
        Self {
            rows: TextRows::new(&body),
            body,
            partial,
        }
    }

    fn show(&self, ui: &mut egui::Ui, colors: bool) {
        let palette = crate::theme::Theme::current_palette(ui.ctx());
        if self.partial {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "包含不完整正文，仅显示已保存内容。",
            );
        }
        egui::ScrollArea::both()
            .id_salt("library_message")
            .show_rows(ui, 18.0, self.rows.count, |ui, rows| {
                for line in self.rows.rows(&self.body, rows) {
                    let mut text =
                        egui::RichText::new(line.trim_end_matches(['\r', '\n'])).monospace();
                    if colors {
                        text = match line.as_bytes().first() {
                            Some(b'+') => text.color(palette.green),
                            Some(b'-') => text.color(palette.red),
                            _ => text,
                        };
                    }
                    ui.add(egui::Label::new(text).extend());
                }
            });
    }
}

impl LibraryUi {
    pub(super) fn is_open(&self) -> bool {
        self.open
    }

    pub(super) fn open(&mut self, compare: bool) {
        self.open = true;
        self.compare = compare;
        self.error = None;
        self.preview = None;
        self.diff = None;
    }

    pub(super) fn close(&mut self) {
        self.open = false;
        self.edit = None;
        self.preview = None;
        self.diff = None;
        if self.reply.is_none() && self.file.is_none() {
            self.archive = None;
        }
    }

    pub(super) fn poll(&mut self, worker: &Worker) {
        if let Some((kind, result)) = self.reply.as_mut().and_then(|(kind, reply)| {
            finished(reply).map(|result| (std::mem::replace(kind, Reply::Load), result))
        }) {
            self.reply = None;
            match result {
                Ok(mut value) => match kind {
                    Reply::Load | Reply::Save => {
                        let document = value["document"].take();
                        self.bookmarks = list(&document, "bookmarks")
                            .iter()
                            .rev()
                            .take(20)
                            .map(|item| {
                                (
                                    label(&item["id"]).to_owned(),
                                    label(&item["name"]).to_owned(),
                                )
                            })
                            .collect();
                        self.comparison_count = list(&document, "comparisonPool").len();
                        self.archive = Some(document);
                        self.diff = None;
                        self.preview = None;
                        self.error = None;
                        if matches!(kind, Reply::Save) {
                            self.edit = None;
                        }
                        if let Some((ids, compare, compare_now)) = self.queued.take() {
                            self.fetch_snapshots(worker, ids, compare, compare_now);
                        }
                    }
                    Reply::Snapshots {
                        compare,
                        compare_now,
                    } => {
                        self.add_snapshots(worker, value["sessions"].take(), compare, compare_now);
                    }
                },
                Err(error) => {
                    self.queued = None;
                    self.error = Some(error);
                }
            }
        }
        self.finish_file(worker);
        if !self.open && self.reply.is_none() && self.file.is_none() {
            self.archive = None;
        }
    }

    fn send(&mut self, worker: &Worker, method: &str, path: &str, body: Value, kind: Reply) {
        match worker.request(method, path, body) {
            Ok(reply) => {
                self.reply = Some((kind, reply));
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }

    fn load(&mut self, worker: &Worker) {
        if self.archive.is_none() && self.reply.is_none() && self.error.is_none() {
            self.send(worker, "GET", ARCHIVE, Value::Null, Reply::Load);
        }
    }

    fn save(&mut self, worker: &Worker, next: Value) {
        if let Some(expected) = &self.archive {
            self.send(
                worker,
                "PUT",
                ARCHIVE,
                json!({"document": next, "expected": expected}),
                Reply::Save,
            );
        }
    }

    pub(super) fn add(
        &mut self,
        worker: &Worker,
        captures: &[&Row],
        compare: bool,
        compare_now: bool,
    ) {
        if self.queued.is_some()
            || self.file.is_some()
            || self
                .reply
                .as_ref()
                .is_some_and(|(kind, _)| !matches!(kind, Reply::Load))
        {
            self.error = Some("书签库正在保存，请稍后重试。".into());
            return;
        }
        if captures.is_empty() || compare_now && captures.len() != 2 {
            self.error = Some("请选择两条记录对比。".into());
            return;
        }
        if captures.iter().any(|row| row.incognito) {
            self.error = Some("无痕记录不能保存到书签库。".into());
            return;
        }
        let ids = captures
            .iter()
            .map(|row| row.id.clone())
            .collect::<Vec<_>>();
        self.open(compare);
        if self.archive.is_some() && self.reply.is_none() {
            self.fetch_snapshots(worker, ids, compare, compare_now);
        } else {
            self.queued = Some((ids, compare, compare_now));
            self.load(worker);
        }
    }

    fn fetch_snapshots(
        &mut self,
        worker: &Worker,
        ids: Vec<String>,
        compare: bool,
        compare_now: bool,
    ) {
        self.send(
            worker,
            "POST",
            "/library/snapshots",
            json!({"ids": ids}),
            Reply::Snapshots {
                compare,
                compare_now,
            },
        );
    }

    fn add_snapshots(
        &mut self,
        worker: &Worker,
        sessions: Value,
        compare: bool,
        compare_now: bool,
    ) {
        let Some(mut next) = self.archive.clone() else {
            self.error = Some("请先加载书签库，再保存快照。".into());
            return;
        };
        let Value::Array(sessions) = sessions else {
            return;
        };
        if compare {
            if compare_now {
                next["comparisonPair"] = json!(
                    sessions
                        .iter()
                        .filter_map(|item| item["id"].as_str())
                        .collect::<Vec<_>>()
                );
            }
            let pool = next["comparisonPool"].as_array_mut().unwrap();
            for session in sessions {
                pool.retain(|entry| entry["id"] != session["id"]);
                pool.push(session);
            }
        } else {
            let bookmarks = next["bookmarks"].as_array_mut().unwrap();
            for session in sessions {
                let name = title(&session);
                let id = uuid::Uuid::new_v4().to_string();
                self.selected.clone_from(&id);
                bookmarks
                    .push(json!({"id": id, "folder":"收件箱", "name": name, "session": session}));
            }
            self.folder.clear();
        }
        self.save(worker, next);
    }

    pub(super) fn show_sidebar(&mut self, ui: &mut egui::Ui, _: &Worker) {
        use crate::workbench::sidebar_row;

        let pool = self.comparison_count;
        egui::CollapsingHeader::new(egui::RichText::new("收藏").strong())
            .default_open(true)
            .show(ui, |ui| {
                if sidebar_row(ui, format!("对比池 ({pool})"), None).clicked() {
                    self.open(true);
                }
            });
        egui::CollapsingHeader::new(egui::RichText::new("书签").strong())
            .default_open(true)
            .show(ui, |ui| {
                if sidebar_row(ui, "我的书签", None).clicked() {
                    self.open(false);
                }
                let mut selected = None;
                for (id, name) in &self.bookmarks {
                    let active = self.open && !self.compare && self.selected == *id;
                    if sidebar_row(ui, name, Some(active))
                        .on_hover_text(name)
                        .clicked()
                    {
                        selected = Some(id.clone());
                    }
                }
                if let Some(selected) = selected {
                    self.open(false);
                    self.selected = selected;
                    self.folder.clear();
                }
            });
    }

    pub(super) fn show(&mut self, ui: &mut egui::Ui, worker: &Worker) -> Option<Value> {
        self.load(worker);
        if ui.small_button("返回抓包").clicked() {
            self.close();
            return None;
        }
        ui.horizontal_wrapped(|ui| {
            ui.heading("书签与对比池");
            if ui.selectable_label(!self.compare, "书签").clicked() {
                self.compare = false;
                self.selected.clear();
                self.preview = None;
            }
            if ui.selectable_label(self.compare, "对比池").clicked() {
                self.compare = true;
                self.selected.clear();
                self.preview = None;
            }
            if ui
                .add_enabled(self.reply.is_none(), egui::Button::new("刷新书签库"))
                .clicked()
            {
                self.send(worker, "GET", ARCHIVE, Value::Null, Reply::Load);
            }
        });
        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        let Some(archive) = &self.archive else {
            if self.reply.is_some() {
                ui.spinner();
            }
            return None;
        };
        let folders: BTreeSet<&str> = list(archive, "folders")
            .iter()
            .filter_map(Value::as_str)
            .chain(
                list(archive, "bookmarks")
                    .iter()
                    .filter_map(|item| item["folder"].as_str()),
            )
            .collect();
        let ready = self.reply.is_none() && self.file.is_none();
        ui.horizontal_wrapped(|ui| {
            if !self.compare {
                egui::ComboBox::from_label("文件夹")
                    .selected_text(if self.folder.is_empty() {
                        "全部"
                    } else {
                        &self.folder
                    })
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(self.folder.is_empty(), "全部")
                            .clicked()
                        {
                            self.folder.clear();
                        }
                        for folder in folders {
                            if ui.selectable_label(self.folder == folder, folder).clicked() {
                                self.folder = folder.to_owned();
                            }
                        }
                    });
                if ui
                    .add_enabled(ready, egui::Button::new("新建文件夹"))
                    .clicked()
                {
                    self.edit = Some(Edit::AddFolder(String::new()));
                }
                if ui
                    .add_enabled(
                        ready && !self.folder.is_empty(),
                        egui::Button::new("删除文件夹"),
                    )
                    .clicked()
                {
                    self.edit = Some(Edit::RemoveFolder(self.folder.clone()));
                }
            }
            for (caption, export) in [("导入书签库", false), ("导出书签库", true)] {
                if ui.add_enabled(ready, egui::Button::new(caption)).clicked() {
                    self.file = Some((export, crate::files::pick("json", export, ui.ctx())));
                }
            }
        });
        ui.separator();
        let (items, pair) = (
            list(
                archive,
                if self.compare {
                    "comparisonPool"
                } else {
                    "bookmarks"
                },
            ),
            list(archive, "comparisonPair"),
        );
        let mut replay = None;
        let mut change_pair = None;
        let mut to_pool = None;
        egui::Panel::left("library_entries")
            .default_size((ui.available_width() * 0.43).max(230.0))
            .resizable(true)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("library_list")
                    .show(ui, |ui| {
                        for item in items {
                            let folder = label(&item["folder"]);
                            if !self.compare
                                && !self.folder.is_empty()
                                && !within(folder, &self.folder)
                            {
                                continue;
                            }
                            let id = label(&item["id"]);
                            let session = if self.compare { item } else { &item["session"] };
                            ui.push_id(id, |ui| {
                                ui.horizontal(|ui| {
                                    if self.compare {
                                        let mut active = pair.iter().any(|entry| entry == id);
                                        if ui
                                            .add_enabled(
                                                ready,
                                                egui::Checkbox::without_text(&mut active),
                                            )
                                            .changed()
                                        {
                                            change_pair = Some((id.to_owned(), active));
                                        }
                                    }
                                    let caption = if self.compare {
                                        title(item)
                                    } else {
                                        label(&item["name"]).to_owned()
                                    };
                                    let response = ui.add(
                                        egui::Button::selectable(self.selected == id, caption)
                                            .truncate(),
                                    );
                                    if response.clicked() {
                                        self.selected = id.to_owned();
                                        self.preview = None;
                                    }
                                    response.context_menu(|ui| {
                                        if ui.button("编辑请求").clicked() {
                                            replay = Some(session.clone());
                                            ui.close();
                                        }
                                        if !self.compare && ready {
                                            if ui.button("重命名或移动").clicked() {
                                                self.edit = Some(Edit::Bookmark {
                                                    id: id.to_owned(),
                                                    name: label(&item["name"]).to_owned(),
                                                    folder: folder.to_owned(),
                                                });
                                                ui.close();
                                            }
                                            if ui.button("加入对比池").clicked() {
                                                to_pool = Some(session.clone());
                                                ui.close();
                                            }
                                        }
                                        if ready && ui.button("删除").clicked() {
                                            self.edit = Some(Edit::Remove {
                                                id: id.to_owned(),
                                                compare: self.compare,
                                            });
                                            ui.close();
                                        }
                                    });
                                });
                                if !self.compare && !folder.is_empty() {
                                    ui.weak(folder);
                                }
                            });
                            ui.separator();
                        }
                    });
            });
        if self.compare {
            if self.diff.is_none() {
                let selected: Vec<_> = pair
                    .iter()
                    .filter_map(|id| items.iter().find(|item| item["id"] == *id))
                    .collect();
                if let [left, right] = selected.as_slice() {
                    self.diff = Some(Preview::new(
                        line_diff(&message(left), &message(right)),
                        incomplete(left) || incomplete(right),
                    ));
                }
            }
            if let Some(diff) = &self.diff {
                diff.show(ui, true);
            } else {
                ui.weak("选择两条记录逐行对比。");
            }
        } else if let Some(entry) = items.iter().find(|item| item["id"] == self.selected) {
            let session = &entry["session"];
            if self.preview.is_none() {
                self.preview = Some(Preview::new(message(session), incomplete(session)));
            }
            ui.heading(label(&entry["name"]));
            if ui.button("编辑请求").clicked() {
                replay = Some(session.clone());
            }
            self.preview.as_ref().unwrap().show(ui, false);
        } else {
            ui.weak("选择书签查看已保存的请求和响应。");
        }
        if let Some((id, enabled)) = change_pair {
            let mut next = archive.clone();
            let mut updated = pair.to_vec();
            updated.retain(|value| value != &id);
            if enabled {
                while updated.len() >= 2 {
                    updated.remove(0);
                }
                updated.push(json!(id));
            }
            next["comparisonPair"] = json!(updated);
            self.save(worker, next);
        } else if let Some(session) = to_pool {
            self.add_snapshots(worker, json!([session]), true, false);
        }
        self.edit_dialog(ui.ctx(), worker);
        replay
    }

    fn edit_dialog(&mut self, ctx: &egui::Context, worker: &Worker) {
        let Some(editor) = &mut self.edit else { return };
        let busy = self.reply.is_some();
        let reply = egui::Modal::new(egui::Id::new("edit_library")).show(ctx, |ui| {
            ui.set_min_width(320.0);
            let valid = match editor {
                Edit::Bookmark { name, folder, .. } => {
                    ui.heading("编辑书签");
                    ui.label("名称");
                    ui.text_edit_singleline(name);
                    ui.label("文件夹");
                    ui.text_edit_singleline(folder);
                    !name.trim().is_empty()
                }
                Edit::AddFolder(name) => {
                    ui.heading("新建文件夹");
                    ui.add(egui::TextEdit::singleline(name).hint_text("使用 / 建立层级"));
                    !name.trim_matches([' ', '/']).is_empty()
                }
                Edit::RemoveFolder(name) => {
                    ui.heading("删除文件夹");
                    ui.label(format!("删除「{name}」和其中的书签？"));
                    true
                }
                Edit::Remove { compare, .. } => {
                    ui.heading(if *compare {
                        "移出对比池"
                    } else {
                        "删除书签"
                    });
                    ui.label("此操作仅删除书签库中的快照。");
                    true
                }
            };
            if let Some(error) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            ui.horizontal(|ui| {
                (
                    ui.add_enabled(!busy, egui::Button::new("取消")).clicked(),
                    ui.add_enabled(!busy && valid, egui::Button::new("确认"))
                        .clicked(),
                )
            })
            .inner
        });
        if reply.inner.0 || !busy && reply.should_close() {
            self.edit = None;
        } else if reply.inner.1 {
            let Some(mut next) = self.archive.clone() else {
                return;
            };
            match editor {
                Edit::Bookmark { id, name, folder } => {
                    if let Some(item) = next["bookmarks"]
                        .as_array_mut()
                        .unwrap()
                        .iter_mut()
                        .find(|item| item["id"] == id.as_str())
                    {
                        item["name"] = json!(name.trim());
                        item["folder"] = json!(folder.trim().trim_matches('/'));
                    }
                }
                Edit::AddFolder(name) => {
                    let name = name.trim().trim_matches('/');
                    if !list(&next, "folders").iter().any(|item| item == name) {
                        if !next["folders"].is_array() {
                            next["folders"] = json!([]);
                        }
                        next["folders"].as_array_mut().unwrap().push(json!(name));
                    }
                }
                Edit::RemoveFolder(name) => {
                    next["bookmarks"]
                        .as_array_mut()
                        .unwrap()
                        .retain(|item| !within(label(&item["folder"]), name));
                    if let Some(folders) = next["folders"].as_array_mut() {
                        folders.retain(|item| !within(label(item), name));
                    }
                    self.folder.clear();
                }
                Edit::Remove { id, compare } => {
                    next[if *compare {
                        "comparisonPool"
                    } else {
                        "bookmarks"
                    }]
                    .as_array_mut()
                    .unwrap()
                    .retain(|item| item["id"] != id.as_str());
                    if *compare && let Some(pair) = next["comparisonPair"].as_array_mut() {
                        pair.retain(|item| item != id.as_str());
                    }
                }
            }
            self.save(worker, next);
        }
    }

    fn finish_file(&mut self, worker: &Worker) {
        let Some((export, result)) = self
            .file
            .as_mut()
            .and_then(|(export, reply)| finished(reply).map(|result| (*export, result)))
        else {
            return;
        };
        self.file = None;
        let result = (|| -> Result<(), String> {
            let Some(path) = result? else { return Ok(()) };
            let Some(mut next) = self.archive.clone() else {
                return Ok(());
            };
            if export {
                let bytes = serde_json::to_vec_pretty(&next).map_err(|err| err.to_string())?;
                return recho_core::native::write_local_file(&path, &bytes);
            }
            let mut bytes = Vec::new();
            std::fs::File::open(path)
                .map_err(|err| err.to_string())?
                .take(EXPORT_LIMIT + 1)
                .read_to_end(&mut bytes)
                .map_err(|err| err.to_string())?;
            if bytes.len() as u64 > EXPORT_LIMIT {
                return Err("书签库文件超过 64 MiB。".into());
            }
            let imported: Value = serde_json::from_slice(&bytes).map_err(|err| err.to_string())?;
            for (key, valid) in [
                ("bookmarks", valid_bookmark as fn(&Value) -> bool),
                ("comparisonPool", valid_snapshot),
            ] {
                let entries = imported[key].as_array().ok_or("书签库配置缺少列表。")?;
                let dest = next[key].as_array_mut().ok_or("本地书签库配置不完整。")?;
                for item in entries {
                    if !valid(item) {
                        return Err("导入的书签或报文快照格式无效。".into());
                    }
                    let mut item = item.clone();
                    if key == "bookmarks" {
                        item["id"] = json!(uuid::Uuid::new_v4().to_string());
                    } else if dest.iter().any(|old| old["id"] == item["id"]) {
                        continue;
                    }
                    dest.push(item);
                }
            }
            let folders = list(&imported, "folders").iter().filter_map(Value::as_str);
            let mut merged: BTreeSet<_> = list(&next, "folders")
                .iter()
                .filter_map(Value::as_str)
                .collect();
            merged.extend(folders);
            next["folders"] = json!(merged);
            self.save(worker, next);
            Ok(())
        })();
        if let Err(error) = result {
            self.error = Some(error);
        }
    }
}

fn list<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value[key].as_array().map_or(&[], Vec::as_slice)
}

fn within(name: &str, parent: &str) -> bool {
    name == parent
        || name
            .strip_prefix(parent)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn valid_snapshot(value: &Value) -> bool {
    ["id", "method", "url"]
        .iter()
        .all(|key| value[*key].as_str().is_some_and(|text| !text.is_empty()))
        && value["status"].as_u64().is_some()
        && value["incognito"] != true
}

fn valid_bookmark(value: &Value) -> bool {
    value["name"].is_string() && value["folder"].is_string() && valid_snapshot(&value["session"])
}

fn title(value: &Value) -> String {
    format!("{} {}", label(&value["method"]), label(&value["url"]))
}

fn incomplete(value: &Value) -> bool {
    value["requestBodyComplete"] != true || value["responseBodyComplete"] != true
}

fn message(value: &Value) -> String {
    let body = |stage: &str| {
        let preview = label(&value[format!("{stage}Body")]);
        if let Some(encoded) = value[format!("{stage}BodyBase64")].as_str() {
            // shortcut: very large snapshot bodies use the archived preview until block-wise decoding is available.
            if encoded.len() > DIFF_LIMIT {
                return format!("{preview}\n[完整正文超过显示上限]");
            }
            if let Ok(bytes) = STANDARD.decode(encoded) {
                if !bytes.contains(&0)
                    && let Ok(text) = std::str::from_utf8(&bytes)
                {
                    return text.to_owned();
                }
            }
            return format!("[Base64]\n{encoded}");
        }
        preview.to_owned()
    };
    format!(
        "{}\n{}\n\n{}\n\n状态 {}\n{}\n\n{}",
        title(value),
        label(&value["requestHeaders"]),
        body("request"),
        value["status"],
        label(&value["responseHeaders"]),
        body("response")
    )
}

fn line_diff(before: &str, after: &str) -> String {
    let left: Vec<_> = before.split('\n').collect();
    let right: Vec<_> = after.split('\n').collect();
    let width = right.len() + 1;
    let cells = (left.len() + 1).saturating_mul(width);
    if cells > 1_000_000 || before.len().saturating_add(after.len()) > 2 * DIFF_LIMIT {
        return "报文超过逐行对比上限。".into();
    }
    let mut lengths = vec![0u32; cells];
    for i in (0..left.len()).rev() {
        for j in (0..right.len()).rev() {
            lengths[i * width + j] = if left[i] == right[j] {
                1 + lengths[(i + 1) * width + j + 1]
            } else {
                lengths[(i + 1) * width + j].max(lengths[i * width + j + 1])
            };
        }
    }
    let mut output = String::with_capacity(before.len() + after.len());
    let (mut i, mut j) = (0, 0);
    while i < left.len() || j < right.len() {
        let (prefix, line) = if i < left.len() && j < right.len() && left[i] == right[j] {
            i += 1;
            j += 1;
            ("  ", left[i - 1])
        } else if j < right.len()
            && (i == left.len() || lengths[i * width + j + 1] > lengths[(i + 1) * width + j])
        {
            j += 1;
            ("+ ", right[j - 1])
        } else {
            i += 1;
            ("- ", left[i - 1])
        };
        output.push_str(prefix);
        output.push_str(line);
        output.push('\n');
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_diff_aligns_later_common_lines() {
        assert_eq!(
            line_diff("same\nold\ntail", "same\nnew\ntail"),
            "  same\n- old\n+ new\n  tail\n"
        );
        assert_eq!(
            line_diff(&"a\n".repeat(1_000), &"b\n".repeat(1_000)),
            "报文超过逐行对比上限。"
        );
    }
}
