use crate::worker::MatchHandle;
use crate::{
    Recho, Row,
    worker::{HistoryHandle, RequestHandle},
};
use eframe::egui;
use recho_core::native::ConditionSet;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Space {
    pub name: String,
    pub query: String,
    pub application: String,
    pub host: String,
    pub protocol: String,
    pub version: String,
    pub content_type: String,
    pub status: u16,
    pub selection: BTreeSet<String>,
    pub detail: Option<String>,
    pub details_open: bool,
    pub descending: bool,
    pub sort: usize,
    pub cleared_through: String,
    pub hidden: BTreeSet<String>,
    pub conditions: ConditionSet,
    pub highlights: ConditionSet,
    pub highlight_color: Option<String>,
}

impl Space {
    pub fn highlight_color(&self) -> &str {
        self.highlight_color.as_deref().unwrap_or("蓝色")
    }
}

pub(super) struct Spaces {
    path: PathBuf,
    pub group: String,
    pub incognito: bool,
    pub items: BTreeMap<String, Space>,
    saved: BTreeMap<String, Space>,
    pub load: Option<HistoryHandle>,
    pub rows: Vec<Row>,
    pub settings: Option<RequestHandle>,
    save_at: Option<Instant>,
    pub error: Option<String>,
    volatile_hidden: BTreeSet<String>,
    match_job: Option<MatchHandle>,
    pub filter_draft: ConditionSet,
    pub filter_error: Option<String>,
    pub matches: Option<BTreeSet<String>>,
    pub marked: BTreeSet<String>,
}

impl Spaces {
    pub fn new(path: PathBuf) -> Self {
        let result = (|| -> Result<BTreeMap<String, Space>, String> {
            let file = match std::fs::File::open(&path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(BTreeMap::new());
                }
                Err(error) => return Err(error.to_string()),
            };
            let mut data = Vec::new();
            file.take(4 * 1024 * 1024 + 1)
                .read_to_end(&mut data)
                .map_err(|error| error.to_string())?;
            if data.len() > 4 * 1024 * 1024 {
                return Err("抓包工作区超过 4 MiB".into());
            }
            serde_json::from_slice(&data).map_err(|error| error.to_string())
        })();
        let (items, error) = match result {
            Ok(items) => (items, None),
            Err(error) => (
                BTreeMap::new(),
                Some(format!("读取抓包工作区失败，原文件已保留：{error}")),
            ),
        };
        Self {
            path,
            group: String::new(),
            incognito: false,
            saved: items.clone(),
            items,
            load: None,
            settings: None,
            rows: vec![],
            save_at: None,
            error,
            volatile_hidden: BTreeSet::new(),
            match_job: None,
            filter_draft: ConditionSet::default(),
            filter_error: None,
            matches: None,
            marked: BTreeSet::new(),
        }
    }
    pub fn current(&self) -> Option<&Space> {
        self.items.get(&self.group)
    }
    fn save(&mut self) -> Result<(), String> {
        let bytes = serde_json::to_vec(&self.items).map_err(|error| error.to_string())?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err("抓包工作区超过 4 MiB".into());
        }
        recho_core::native::write_local_file(&self.path, &bytes)
    }
    pub fn persist(&mut self, ctx: &egui::Context, flush: bool) {
        if self.saved == self.items || self.error.is_some() || self.group.is_empty() {
            return;
        }
        let due = *self
            .save_at
            .get_or_insert_with(|| Instant::now() + Duration::from_millis(400));
        if flush || due <= Instant::now() {
            match self.save() {
                Ok(()) => self.saved.clone_from(&self.items),
                Err(error) => self.error = Some(format!("保存抓包工作区失败：{error}")),
            }
            self.save_at = None;
        } else {
            ctx.request_repaint_after(due.saturating_duration_since(Instant::now()));
        }
    }
}

impl Recho {
    pub(super) fn remember_capture(&mut self) {
        if self.navigation == crate::Navigation::History || self.spaces.group.is_empty() {
            return;
        }
        let Some(space) = self.spaces.items.get_mut(&self.spaces.group) else {
            return;
        };
        space.query.clone_from(&self.filter);
        space.application.clone_from(&self.app_filter);
        space.host.clone_from(&self.host_filter);
        space.protocol.clone_from(&self.protocol_filter);
        space.version.clone_from(&self.version_filter);
        space.content_type.clone_from(&self.type_filter);
        space.status = self.status_filter;
        space.sort = self.sort_column;
        space.descending = !self.ascending;
        if self.spaces.load.is_none() {
            space.selection = self
                .selection
                .iter()
                .filter(|id| self.rows.iter().any(|row| &row.id == *id && !row.incognito))
                .cloned()
                .collect();
            space.detail = self
                .selected
                .as_ref()
                .filter(|id| space.selection.contains(*id))
                .cloned();
            space.details_open = self.details_open;
        }
    }

    pub(super) fn open_capture_space(&mut self, group: String) {
        if self.spaces.group == group {
            return;
        }
        self.remember_capture();
        self.spaces.group = group;
        self.spaces.rows.clear();
        self.spaces.match_job = None;
        self.spaces.matches = None;
        self.spaces.marked.clear();
        self.highlight_editor = None;
        let group = self.spaces.group.clone();
        let state = self
            .spaces
            .items
            .entry(group.clone())
            .or_insert_with(|| Space {
                name: if group == "default" {
                    "调试".into()
                } else {
                    "调试会话".into()
                },
                ..Space::default()
            })
            .clone();
        self.filter = state.query;
        self.app_filter = state.application;
        self.host_filter = state.host;
        (self.protocol_filter, self.version_filter) = match state.protocol.as_str() {
            "HTTP1" | "HTTP2" | "HTTP3" => (String::new(), state.protocol),
            _ => (state.protocol, state.version),
        };
        self.type_filter = state.content_type;
        self.status_filter = state.status;
        self.sort_column = state.sort;
        self.ascending = !state.descending;
        self.spaces.filter_draft = state.conditions;
        self.spaces.filter_error = self.spaces.filter_draft.validate().err();
        if self.navigation != crate::Navigation::History {
            self.select(None);
            self.details_open = state.details_open;
        }
        match self.worker.history(0, 200, String::new(), Some(group)) {
            Ok(reply) => self.spaces.load = Some(reply),
            Err(error) => self.error = Some(error),
        }
        self.rebuild_capture_rows();
    }

    pub(super) fn rebuild_capture_rows(&mut self) {
        let group = &self.spaces.group;
        self.spaces
            .volatile_hidden
            .retain(|id| self.live_rows.iter().any(|row| &row.id == id));
        let Some(space) = self.spaces.current() else {
            return;
        };
        let mut ids = BTreeSet::new();
        let mut rows: Vec<_> = self
            .live_rows
            .iter()
            .chain(&self.spaces.rows)
            .filter(|row| {
                &row.group == group
                    && row.started_at > space.cleared_through
                    && !space.hidden.contains(&row.id)
                    && !self.spaces.volatile_hidden.contains(&row.id)
                    && ids.insert(row.id.clone())
            })
            .cloned()
            .collect();
        rows.sort_by(|a, b| a.started_at.cmp(&b.started_at));
        if rows.len() > 200 {
            rows.drain(..rows.len() - 200);
        }
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
        self.evaluate_conditions();
        if self.navigation != crate::Navigation::History {
            self.selection
                .retain(|id| self.rows.iter().any(|row| &row.id == id));
            if self
                .selected
                .as_ref()
                .is_some_and(|id| !self.selection.contains(id))
            {
                self.select_detail(self.selection.iter().next().cloned());
            }
        }
    }

    pub(super) fn evaluate_conditions(&mut self) {
        let Some(space) = self.spaces.current() else {
            return;
        };
        let filtering = space.conditions.active();
        let highlighting = space.highlights.active();
        if filtering || highlighting {
            match self.worker.filter_captures(
                self.rows.iter().map(|row| row.id.clone()).collect(),
                space.conditions.clone(),
                space.highlights.clone(),
                space.highlight_color().into(),
            ) {
                Ok(reply) => self.spaces.match_job = Some(reply),
                Err(error) => self.error = Some(error),
            }
        } else {
            self.spaces.match_job = None;
        }
        if !highlighting {
            self.spaces.marked.clear();
        }
        if !filtering {
            self.spaces.matches = None;
            self.refilter();
        }
    }

    pub(super) fn activate_capture(&mut self, group: String) {
        if self.spaces.settings.is_some() || group == self.spaces.group {
            return;
        }
        let expected = &self.spaces.group;
        match self.worker.request(
            "PATCH",
            "/settings",
            serde_json::json!({
                "captureGroup": group, "_expected": {"captureGroup": expected}
            }),
        ) {
            Ok(reply) => self.spaces.settings = Some(reply),
            Err(error) => self.error = Some(error),
        }
    }

    pub(super) fn new_capture_space(&mut self) {
        if self.spaces.items.len() >= 32 {
            self.error = Some("最多可保留 32 个调试标签".into());
            return;
        }
        let group = uuid::Uuid::new_v4().to_string();
        let name = format!("调试 {}", self.spaces.items.len() + 1);
        self.spaces.items.insert(
            group.clone(),
            Space {
                name,
                ..Space::default()
            },
        );
        self.activate_capture(group);
    }

    pub(super) fn clear_capture_space(&mut self) {
        if let Some(space) = self.spaces.items.get_mut(&self.spaces.group) {
            if let Some(newest) = self.rows.iter().rev().find(|row| !row.incognito) {
                space.cleared_through.clone_from(&newest.started_at);
            }
            space.hidden.clear();
        }
        let group = self.spaces.group.clone();
        self.spaces.volatile_hidden.extend(
            self.live_rows
                .iter()
                .filter(|row| row.group == group && row.incognito)
                .map(|row| row.id.clone()),
        );
        self.send(crate::Command::Clear(group));
        self.select(None);
        self.rebuild_capture_rows();
    }

    pub(super) fn hide_selection(&mut self) {
        if let Some(space) = self.spaces.items.get_mut(&self.spaces.group) {
            space.hidden.extend(
                self.rows
                    .iter()
                    .filter(|row| self.selection.contains(&row.id) && !row.incognito)
                    .map(|row| row.id.clone()),
            );
        }
        self.spaces.volatile_hidden.extend(
            self.live_rows
                .iter()
                .filter(|row| row.incognito && self.selection.contains(&row.id))
                .map(|row| row.id.clone()),
        );
        self.select(None);
        self.rebuild_capture_rows();
    }

    pub(super) fn poll_capture_spaces(&mut self) {
        use tokio::sync::oneshot::error::TryRecvError;
        if let Some(job) = &mut self.spaces.match_job {
            match job.try_recv() {
                Ok(Ok(matches)) => {
                    self.spaces.match_job = None;
                    self.spaces.matches = matches.visible.map(|ids| ids.into_iter().collect());
                    self.spaces.marked = matches.marked.into_iter().collect();
                    self.refilter();
                }
                Ok(Err(error)) => {
                    self.spaces.match_job = None;
                    self.error = Some(error);
                }
                Err(TryRecvError::Empty) => {}
                Err(error) => {
                    self.spaces.match_job = None;
                    self.error = Some(error.to_string());
                }
            }
        }
        if let Some(reply) = &mut self.spaces.settings {
            match reply.try_recv() {
                Ok(result) => {
                    self.spaces.settings = None;
                    if let Err(error) = result {
                        self.error = Some(error);
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(error) => {
                    self.spaces.settings = None;
                    self.error = Some(error.to_string());
                }
            }
        }
        if let Some(reply) = &mut self.spaces.load {
            let result = match reply.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => return,
                Err(error) => Err(error.to_string()),
            };
            self.spaces.load = None;
            match result {
                Ok(rows) => {
                    self.spaces.rows = rows;
                    self.rebuild_capture_rows();
                    if self.navigation != crate::Navigation::History && self.selected.is_none() {
                        if let Some(space) = self.spaces.current() {
                            let selection = space
                                .selection
                                .iter()
                                .filter(|id| self.rows.iter().any(|row| &row.id == *id))
                                .cloned()
                                .collect();
                            let detail = space
                                .detail
                                .clone()
                                .filter(|id| self.rows.iter().any(|row| &row.id == id));
                            self.selection = selection;
                            self.select_detail(detail);
                        }
                    }
                }
                Err(error) => self.error = Some(error),
            }
        }
    }
}
