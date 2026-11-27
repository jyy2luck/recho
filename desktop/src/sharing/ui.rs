use crate::{
    files::{self, FilePick},
    theme::{Theme, dialog_footer, dialog_header},
};
use eframe::egui;
use tokio::sync::oneshot::error::TryRecvError;

pub(crate) struct ShareUi<T> {
    import: Option<Import<T>>,
    export: Option<(FilePick, String)>,
    notice: Option<Result<String, String>>,
}

struct Import<T> {
    title: &'static str,
    text: String,
    file: Option<FilePick>,
    preview: Option<(T, String)>,
    error: Option<String>,
    focus_text: bool,
}

impl<T> Default for ShareUi<T> {
    fn default() -> Self {
        Self {
            import: None,
            export: None,
            notice: None,
        }
    }
}

impl<T> ShareUi<T> {
    pub(crate) fn notice(&self) -> Option<&Result<String, String>> {
        self.notice.as_ref()
    }

    pub(crate) fn is_open(&self) -> bool {
        self.import.is_some() || self.export.is_some()
    }

    pub(crate) fn open(&mut self, title: &'static str) {
        if !self.is_open() {
            self.notice = None;
            self.import = Some(Import {
                title,
                text: String::new(),
                file: None,
                preview: None,
                error: None,
                focus_text: true,
            });
        }
    }

    pub(crate) fn export_menu(
        &mut self,
        ui: &mut egui::Ui,
        filename: &str,
        encode: impl FnOnce() -> Result<String, String>,
    ) -> bool {
        let copy = ui
            .add_enabled(!self.is_open(), egui::Button::new("复制分享文本"))
            .clicked();
        let export = ui
            .add_enabled(!self.is_open(), egui::Button::new("导出 JSON 文件"))
            .clicked();
        if copy || export {
            self.notice = None;
            match encode() {
                Ok(text) if copy => {
                    ui.ctx().copy_text(text);
                    self.notice = Some(Ok("已复制分享文本".into()));
                }
                Ok(text) => {
                    let picker = files::pick_named("json", filename.into(), ui.ctx());
                    self.export = Some((picker, text));
                }
                Err(error) => self.notice = Some(Err(error)),
            }
        }
        copy || export
    }

    pub(crate) fn poll(
        &mut self,
        ctx: &egui::Context,
        preview: impl Fn(&str) -> Result<(T, String), String>,
    ) -> Option<T> {
        if let Some((picker, text)) = &mut self.export
            && let Some(result) = picked(picker)
        {
            self.notice = match result {
                Ok(Some(path)) => Some(
                    recho_core::native::write_local_file(&path, text.as_bytes())
                        .map(|()| "已导出分享文件".into())
                        .map_err(|error| error.to_string()),
                ),
                Ok(None) => None,
                Err(error) => Some(Err(error)),
            };
            self.export = None;
        }
        let mut draft = self.import.take()?;
        if let Some(picker) = &mut draft.file
            && let Some(result) = picked(picker)
        {
            draft.file = None;
            match result {
                Ok(Some(path)) => {
                    draft.error = None;
                    match super::read(&path).and_then(|text| preview(&text)) {
                        Ok(result) => draft.preview = Some(result),
                        Err(error) => draft.error = Some(error),
                    }
                }
                Ok(None) => {}
                Err(error) => draft.error = Some(error),
            }
        }
        let field_id = egui::Id::new(("share_text", draft.title));
        if draft.preview.is_none()
            && draft.file.is_none()
            && ctx.memory(|memory| memory.has_focus(field_id))
            && let Some(text) = ctx.input_mut(|input| {
                input
                    .events
                    .iter()
                    .position(
                        |event| matches!(event, egui::Event::Paste(text) if text.len() > 64 * 1024),
                    )
                    .and_then(|index| match input.events.remove(index) {
                        egui::Event::Paste(text) => Some(text),
                        _ => None,
                    })
            })
        {
            // Preview large pastes without laying out an entire source document.
            match preview(&text) {
                Ok(result) => {
                    draft.preview = Some(result);
                    draft.error = None;
                }
                Err(error) => draft.error = Some(error),
            }
        }
        let colors = Theme::current_palette(ctx);
        let modal =
            Theme::modal(ctx, egui::Id::new(("import_share", draft.title))).show(ctx, |ui| {
                ui.set_width((ctx.content_rect().width() - 48.0).clamp(280.0, 560.0));
                let close = dialog_header(ui, draft.title, draft.file.is_none());
                match &draft.preview {
                    Some((_, summary)) => {
                        egui::ScrollArea::vertical()
                            .max_height((ui.available_height() * 0.42).clamp(80.0, 300.0))
                            .show(ui, |ui| ui.add(egui::Label::new(summary).selectable(true)));
                    }
                    None => {
                        if ui
                            .add_enabled(draft.file.is_none(), egui::Button::new("打开 JSON 文件…"))
                            .clicked()
                        {
                            draft.file = Some(files::pick("json", false, ctx));
                        }
                        let field = egui::ScrollArea::vertical()
                            .max_height(145.0)
                            .show(ui, |ui| {
                                ui.add(
                                    egui::TextEdit::multiline(&mut draft.text)
                                        .id(field_id)
                                        .font(egui::TextStyle::Monospace)
                                        .hint_text("粘贴分享文本")
                                        .desired_rows(5)
                                        .desired_width(f32::INFINITY),
                                )
                            })
                            .inner;
                        ctx.accesskit_node_builder(field.id, |node| node.set_label("分享文本"));
                        if std::mem::take(&mut draft.focus_text) {
                            field.request_focus();
                        }
                        if field.changed() {
                            draft.error = None;
                        }
                    }
                }
                if let Some(error) = &draft.error {
                    ui.colored_label(colors.red, error);
                }
                dialog_footer(ui, |ui| {
                    if draft.preview.is_some() {
                        let confirm = ui
                            .add(colors.dialog_action("导入", colors.accent))
                            .clicked();
                        if ui.button("返回").clicked() {
                            draft.preview = None;
                        }
                        (close, confirm)
                    } else {
                        if ui
                            .add_enabled(
                                draft.file.is_none() && !draft.text.trim().is_empty(),
                                colors.dialog_action("预览", colors.accent),
                            )
                            .clicked()
                        {
                            match preview(&draft.text) {
                                Ok(result) => {
                                    draft.preview = Some(result);
                                    draft.error = None;
                                }
                                Err(error) => draft.error = Some(error),
                            }
                        }
                        let cancel = ui.button("取消").clicked();
                        (close || cancel, false)
                    }
                })
            });
        let (close, confirm) = modal.inner;
        if confirm {
            return draft.preview.map(|(value, _)| value);
        }
        if draft.file.is_some() || (!close && !modal.should_close()) {
            self.import = Some(draft);
        }
        None
    }
}

fn picked(picker: &mut FilePick) -> Option<Result<Option<std::path::PathBuf>, String>> {
    match picker.try_recv() {
        Ok(result) => Some(result),
        Err(TryRecvError::Empty) => None,
        Err(TryRecvError::Closed) => Some(Err("文件选择已中止。".into())),
    }
}
