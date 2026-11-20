use super::*;
use egui_extras::{Column, TableBuilder};

fn icon(code: &str) -> RichText {
    RichText::new(code).font(egui::FontId::new(
        19.0,
        egui::FontFamily::Name("icons".into()),
    ))
}
fn icon_button(ui: &mut egui::Ui, code: &str, title: &str, enabled: bool) -> egui::Response {
    let response = ui.add_enabled(
        enabled,
        egui::Button::new(icon(code)).min_size(egui::vec2(28.0, 28.0)),
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, title));
    response.on_hover_text(title)
}

fn disabled(ui: &mut egui::Ui, label: &str) {
    ui.add_enabled(false, egui::Button::new(label))
        .on_disabled_hover_text("此功能尚未迁移");
}
fn section(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    egui::CollapsingHeader::new(RichText::new(title).strong())
        .default_open(true)
        .show(ui, body);
}

pub(super) fn show(app: &mut Recho, ui: &mut egui::Ui) {
    if ui
        .ctx()
        .input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::G))
        && app.ready
    {
        app.toggle_capture();
    }
    if ui
        .ctx()
        .input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::F1))
    {
        app.tools_sidebar = false;
        app.sidebar_visible = true;
    }
    if ui
        .ctx()
        .input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
    {
        app.details_open = false;
    }
    egui::Panel::top("menus")
        .frame(
            egui::Frame::new()
                .fill(CHROME)
                .inner_margin(egui::Margin::symmetric(8, 1)),
        )
        .show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.label(RichText::new("recho").strong().color(ACCENT));
                ui.menu_button("文件", |ui| {
                    disabled(ui, "新建 API 请求");
                    disabled(ui, "导入 HAR");
                    if ui.button("退出").clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.menu_button("编辑", |ui| {
                    if ui.button("复制所选 URL").clicked() {
                        copy_url(app, ui);
                        ui.close();
                    }
                });
                ui.menu_button("视图", |ui| {
                    ui.checkbox(&mut app.sidebar_visible, "显示侧边栏");
                    if ui.checkbox(&mut app.details_open, "显示详情").changed() && app.details_open
                    {
                        app.load_details();
                    }
                });
                ui.menu_button("调试", |ui| {
                    if ui
                        .button(if app.running {
                            "停止调试  ⌘/Ctrl+G"
                        } else {
                            "开始调试  ⌘/Ctrl+G"
                        })
                        .clicked()
                    {
                        app.toggle_capture();
                        ui.close();
                    }
                    if ui.button("清空捕获").clicked() {
                        app.clear_confirmation = true;
                        ui.close();
                    }
                });
                ui.menu_button("代理", |ui| {
                    if ui.button("连接设置").clicked() {
                        app.connection_open = true;
                        ui.close();
                    }
                    disabled(ui, "系统代理");
                });
                ui.menu_button("SSL", |ui| {
                    let mut intercept = app.intercept_tls;
                    if ui.checkbox(&mut intercept, "HTTPS 解密").changed() {
                        app.send(Command::Configure {
                            running: app.running,
                            intercept_tls: intercept,
                        });
                    }
                    if ui.button("根证书与连接指引").clicked() {
                        app.connection_open = true;
                        ui.close();
                    }
                });
                ui.menu_button("帮助", |ui| {
                    ui.label("recho 0.1.0 · Rust 桌面验证版");
                    ui.label("核心抓包流程已接入；灰色入口尚未迁移。");
                });
            });
        });
    egui::Panel::bottom("status")
        .frame(
            egui::Frame::new()
                .fill(CHROME)
                .inner_margin(egui::Margin::symmetric(8, 3)),
        )
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .small_button("◧")
                    .on_hover_text("展开或收起侧边栏")
                    .clicked()
                {
                    app.sidebar_visible = !app.sidebar_visible;
                }
                if icon_button(ui, "\u{e869}", "调试工具", true)
                    .on_hover_text("调试工具")
                    .clicked()
                {
                    app.tools_sidebar = true;
                    app.sidebar_visible = true;
                }
                ui.label(format!(
                    "共 {} 项 · 已选择 {}",
                    app.visible.len(),
                    usize::from(app.selected.is_some())
                ));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak("0.1.0 · 本地");
                });
            });
            for error in [&app.history_error, &app.error].into_iter().flatten() {
                ui.colored_label(Color32::LIGHT_RED, error);
            }
            if app.error.is_some() && ui.small_button("关闭提示").clicked() {
                app.error = None;
            }
        });
    egui::Panel::left("rail")
        .exact_size(40.0)
        .frame(egui::Frame::new().fill(CHROME).inner_margin(4))
        .show(ui, |ui| {
            for (icon, title, active, available) in [
                ("\u{e871}", "工作台 (F1)", !app.tools_sidebar, true),
                ("\u{e53b}", "集合 (F2)", false, false),
                ("\u{e24a}", "环境变量 (F3)", false, false),
                ("\u{e889}", "历史 (F4)", false, false),
                ("\u{e869}", "调试工具", app.tools_sidebar, true),
            ] {
                if ui
                    .add_enabled(
                        available,
                        egui::Button::selectable(active, self::icon(icon))
                            .min_size(egui::vec2(30.0, 34.0)),
                    )
                    .on_hover_text(title)
                    .on_disabled_hover_text(format!("{title} · 尚未迁移"))
                    .clicked()
                {
                    app.tools_sidebar = title == "调试工具";
                    app.sidebar_visible = true;
                }
            }
            ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                if ui
                    .add(egui::Button::new(icon("\u{e8b8}")))
                    .on_hover_text("连接设置")
                    .clicked()
                {
                    app.connection_open = true;
                }
            });
        });
    if app.sidebar_visible {
        egui::Panel::left("sidebar")
            .default_size(260.0)
            .min_size(180.0)
            .max_size(400.0)
            .resizable(true)
            .frame(
                egui::Frame::new()
                    .fill(SIDE)
                    .inner_margin(egui::Margin::symmetric(10, 7)),
            )
            .show(ui, |ui| sidebar(app, ui));
    }
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(BG))
        .show(ui, |ui| {
            proxy_bar(app, ui);
            ui.horizontal(|ui| {
                ui.add(
                    egui::Button::selectable(
                        true,
                        RichText::new(format!("◉  调试 ({})", app.rows.len())).color(ACCENT),
                    )
                    .min_size(egui::vec2(100.0, 30.0)),
                );
                disabled(ui, "＋");
            });
            ui.separator();
            filters(app, ui);
            ui.horizontal(|ui| {
                ui.label(icon("\u{e8b6}"));
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut app.filter)
                            .hint_text("搜索 URL、方法、状态")
                            .desired_width((ui.available_width() - 80.0).max(80.0)),
                    )
                    .changed()
                {
                    app.refilter();
                }
                if ui
                    .add(egui::Button::selectable(app.details_open, icon("\u{e949}")))
                    .on_hover_text("显示或关闭详情；双击记录打开")
                    .clicked()
                {
                    app.details_open = !app.details_open;
                    if app.details_open {
                        app.load_details();
                    }
                }
            });
            ui.separator();
            if app.details_open && app.selected.is_some() {
                let size = (ui.available_width() * 0.50).max(360.0);
                egui::Panel::right("details")
                    .default_size(size)
                    .min_size(330.0)
                    .resizable(true)
                    .show(ui, |ui| details(app, ui));
            }
            capture_table(app, ui);
        });
    dialogs(app, ui.ctx());
}

fn sidebar(app: &mut Recho, ui: &mut egui::Ui) {
    ui.weak(if app.tools_sidebar {
        "调试工具"
    } else {
        "工作台"
    });
    ui.separator();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if app.tools_sidebar {
                section(ui, "调试", |ui| {
                    for label in ["重写", "断点", "脚本", "自动高亮"] {
                        disabled(ui, label);
                    }
                });
                section(ui, "代理", |ui| {
                    for label in [
                        "SSL 配置",
                        "访问控制",
                        "二级代理",
                        "网关",
                        "镜像",
                        "反向代理",
                    ] {
                        disabled(ui, label);
                    }
                });
                section(ui, "本地工具", |ui| {
                    disabled(ui, "MCP");
                    if ui.button("证书与连接指引").clicked() {
                        app.connection_open = true;
                    }
                });
            } else {
                section(ui, "规范文件", |ui| {
                    disabled(ui, "Protobuf / gRPC");
                });
                section(ui, "收藏", |ui| disabled(ui, "对比池 (0)"));
                section(ui, "书签", |ui| disabled(ui, "我的书签"));
                section(ui, "设备", |ui| {
                    if ui.button("证书与连接指引").clicked() {
                        app.connection_open = true;
                    }
                });
                let apps: BTreeSet<_> =
                    app.rows.iter().map(|row| row.application.clone()).collect();
                section(ui, "应用程序", |ui| {
                    if apps.is_empty() {
                        ui.weak("暂无应用程序");
                    }
                    if !app.app_filter.is_empty()
                        && ui.selectable_label(false, "全部应用").clicked()
                    {
                        app.app_filter.clear();
                        app.refilter();
                    }
                    for name in apps {
                        if ui.selectable_label(app.app_filter == name, &name).clicked() {
                            app.app_filter = if app.app_filter == name {
                                String::new()
                            } else {
                                name
                            };
                            app.refilter();
                        }
                    }
                });
                let hosts: BTreeSet<_> = app
                    .rows
                    .iter()
                    .map(|row| row.host.clone())
                    .filter(|host| !host.is_empty())
                    .collect();
                section(ui, "域名", |ui| {
                    if !app.host_filter.is_empty()
                        && ui.selectable_label(false, "全部域名").clicked()
                    {
                        app.host_filter.clear();
                        app.refilter();
                    }
                    for host in hosts {
                        if ui
                            .selectable_label(app.host_filter == host, &host)
                            .clicked()
                        {
                            app.host_filter = if app.host_filter == host {
                                String::new()
                            } else {
                                host
                            };
                            app.refilter();
                        }
                    }
                });
            }
        });
}

fn proxy_bar(app: &mut Recho, ui: &mut egui::Ui) {
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(14, 14))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let text = if app.ready {
                    format!("●  Proxying on 127.0.0.1:{}", app.port)
                } else {
                    "正在启动引擎…".into()
                };
                let width = (ui.available_width() - 165.0).max(150.0);
                egui::Frame::new()
                    .fill(CHROME)
                    .stroke(egui::Stroke::new(1.0, Color32::from_rgb(57, 62, 72)))
                    .corner_radius(4)
                    .inner_margin(8)
                    .show(ui, |ui| {
                        ui.set_width((width - 16.0).max(120.0));
                        ui.horizontal(|ui| {
                            if ui
                                .add(
                                    egui::Button::new(RichText::new(text).color(if app.running {
                                        Color32::from_rgb(131, 212, 165)
                                    } else {
                                        Color32::GRAY
                                    }))
                                    .frame(false),
                                )
                                .on_hover_text("修改监听端口")
                                .clicked()
                            {
                                app.connection_open = true;
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if icon_button(ui, "\u{e9e0}", "根证书与连接指引", true)
                                        .on_hover_text("根证书与连接指引")
                                        .clicked()
                                    {
                                        app.connection_open = true;
                                    }
                                    icon_button(ui, "\u{e894}", "系统代理", false)
                                        .on_disabled_hover_text("系统代理尚未迁移");
                                    if icon_button(ui, "\u{e14d}", "复制代理地址", true)
                                        .on_hover_text("复制代理地址")
                                        .clicked()
                                    {
                                        ui.ctx().copy_text(format!("127.0.0.1:{}", app.port));
                                    }
                                },
                            );
                        });
                    });
                if ui
                    .add_enabled(
                        app.ready,
                        egui::Button::new(if app.running {
                            "■  停止"
                        } else {
                            "▶  调试"
                        })
                        .min_size(egui::vec2(75.0, 38.0)),
                    )
                    .clicked()
                {
                    app.toggle_capture();
                }
                if icon_button(ui, "\u{e92e}", "清空捕获", app.ready).clicked() {
                    app.clear_confirmation = true;
                }
            });
        });
}

fn filters(app: &mut Recho, ui: &mut egui::Ui) {
    let mut changed = false;
    egui::ScrollArea::horizontal()
        .id_salt("quick_filters")
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(
                        app.protocol_filter.is_empty()
                            && app.type_filter.is_empty()
                            && app.status_filter == 0,
                        "All",
                    )
                    .clicked()
                {
                    app.protocol_filter.clear();
                    app.type_filter.clear();
                    app.status_filter = 0;
                    changed = true;
                }
                ui.separator();
                for protocol in ["HTTP", "HTTPS", "HTTP1", "HTTP2", "HTTP3"] {
                    if ui
                        .selectable_label(app.protocol_filter == protocol, protocol)
                        .clicked()
                    {
                        app.protocol_filter = if app.protocol_filter == protocol {
                            String::new()
                        } else {
                            protocol.into()
                        };
                        changed = true;
                    }
                }
                ui.separator();
                for (label, content_type) in [
                    ("JSON", "json"),
                    ("XML", "xml"),
                    ("文本", "text/plain"),
                    ("HTML", "html"),
                    ("JS", "javascript"),
                    ("图片", "image/"),
                    ("媒体", "video/"),
                ] {
                    if ui
                        .selectable_label(app.type_filter == content_type, label)
                        .clicked()
                    {
                        app.type_filter = if app.type_filter == content_type {
                            String::new()
                        } else {
                            content_type.into()
                        };
                        changed = true;
                    }
                }
                ui.separator();
                for status in 1..=5 {
                    if ui
                        .selectable_label(app.status_filter == status, format!("{status}xx"))
                        .clicked()
                    {
                        app.status_filter = if app.status_filter == status {
                            0
                        } else {
                            status
                        };
                        changed = true;
                    }
                }
            });
        });
    if changed {
        app.refilter();
    }
}

fn copy_url(app: &Recho, ui: &egui::Ui) {
    if let Some(row) = app
        .rows
        .iter()
        .find(|row| app.selected.as_ref() == Some(&row.id))
    {
        ui.ctx().copy_text(row.url.clone());
    }
}
fn capture_table(app: &mut Recho, ui: &mut egui::Ui) {
    if app.rows.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.vertical_centered(|ui| {
                ui.strong(if app.running {
                    "停止调试                 ⌘ / Ctrl + G"
                } else {
                    "开始调试                 ⌘ / Ctrl + G"
                });
                if ui.button("连接设置").clicked() {
                    app.connection_open = true;
                }
            });
        });
        return;
    }
    let mut choose = None;
    let mut open = false;
    let mut sort = None;
    egui::ScrollArea::horizontal()
        .id_salt("table_horizontal")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_min_width(980.0);
            let table = TableBuilder::new(ui)
                .id_salt("captures")
                .resizable(true)
                .striped(false)
                .sense(egui::Sense::click())
                .auto_shrink([false, false])
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::initial(48.0).clip(true))
                .column(Column::initial(36.0).clip(true))
                .column(Column::initial(62.0).clip(true))
                .column(Column::remainder().at_least(230.0).clip(true))
                .column(Column::initial(108.0).clip(true))
                .column(Column::initial(65.0).clip(true))
                .column(Column::initial(112.0).clip(true))
                .column(Column::initial(75.0).clip(true))
                .column(Column::initial(75.0).clip(true));
            table
                .header(28.0, |mut row| {
                    for (index, title) in [
                        "ID",
                        "图标",
                        "方法",
                        "URL",
                        "应用程序",
                        "状态码",
                        "服务器 IP",
                        "时长",
                        "大小",
                    ]
                    .iter()
                    .enumerate()
                    {
                        row.col(|ui| {
                            if ui
                                .selectable_label(
                                    app.sort_column == index,
                                    format!(
                                        "{title}{}",
                                        if app.sort_column == index {
                                            if app.ascending { " ^" } else { " v" }
                                        } else {
                                            ""
                                        }
                                    ),
                                )
                                .clicked()
                            {
                                sort = Some(index);
                            }
                        });
                    }
                })
                .body(|body| {
                    body.rows(26.0, app.visible.len(), |mut table_row| {
                        let row = &app.rows[app.visible[table_row.index()]];
                        table_row.set_selected(app.selected.as_ref() == Some(&row.id));
                        let color = if row.error.is_some() || row.status >= 400 {
                            Color32::from_rgb(255, 147, 155)
                        } else {
                            Color32::from_rgb(131, 212, 165)
                        };
                        let cells = [
                            app.sequence.get(&row.id).unwrap_or(&0).to_string(),
                            if row.url.starts_with("https:") {
                                "♢".into()
                            } else {
                                "↔".into()
                            },
                            row.method.clone(),
                            row.url.clone(),
                            row.application.clone(),
                            if row.status == 0 {
                                "…".into()
                            } else {
                                row.status.to_string()
                            },
                            row.server_ip.clone(),
                            format!("{} ms", row.duration_ms),
                            format!("{} B", row.response_bytes),
                        ];
                        for (index, text) in cells.into_iter().enumerate() {
                            table_row.col(|ui| {
                                ui.add(
                                    egui::Label::new(RichText::new(text).size(12.0).color(
                                        if index == 5 {
                                            color
                                        } else {
                                            ui.visuals().text_color()
                                        },
                                    ))
                                    .truncate()
                                    .selectable(false),
                                );
                            });
                        }
                        let response = table_row.response();
                        if response.clicked() || response.secondary_clicked() {
                            choose = Some(row.id.clone());
                        }
                        if response.double_clicked() {
                            choose = Some(row.id.clone());
                            open = true;
                        }
                        response.context_menu(|ui| {
                            if ui.button("查看详情").clicked() {
                                choose = Some(row.id.clone());
                                open = true;
                                ui.close();
                            }
                            if ui.button("复制 URL").clicked() {
                                ui.ctx().copy_text(row.url.clone());
                                ui.close();
                            }
                        });
                    });
                });
        });
    if let Some(column) = sort {
        if app.sort_column == column {
            app.ascending = !app.ascending;
        } else {
            app.sort_column = column;
            app.ascending = true;
        }
        app.refilter();
    }
    if open {
        app.details_open = true;
    }
    if let Some(id) = choose {
        app.select(Some(id));
    }
}

fn details(app: &mut Recho, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.label("详情");
        if ui.small_button("重新读取").clicked() {
            app.select(app.selected.clone());
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("×").on_hover_text("关闭详情").clicked() {
                app.details_open = false;
            }
        });
    });
    ui.separator();
    let height = ui.available_height() * 0.48;
    egui::Panel::top("request")
        .default_size(height)
        .min_size(120.0)
        .resizable(true)
        .show(ui, |ui| message(app, ui, 0));
    message(app, ui, 1);
}
fn message(app: &mut Recho, ui: &mut egui::Ui, index: usize) {
    let request = index == 0;
    let Some(row) = app
        .rows
        .iter()
        .find(|row| app.selected.as_ref() == Some(&row.id))
    else {
        return;
    };
    let panel = &mut app.messages[index];
    ui.horizontal(|ui| {
        ui.label(RichText::new(if request { "请求" } else { "响应" }).color(ACCENT));
        if request {
            ui.selectable_value(&mut panel.tab, MessageTab::Overview, "总览");
        }
        ui.selectable_value(&mut panel.tab, MessageTab::Raw, "原始");
        ui.selectable_value(
            &mut panel.tab,
            MessageTab::Headers,
            if request { "请求头" } else { "响应头" },
        );
        ui.selectable_value(
            &mut panel.tab,
            MessageTab::Body,
            if request { "请求体" } else { "响应体" },
        );
    });
    ui.separator();
    ui.add(
        egui::Label::new(if request {
            row.url.clone()
        } else {
            format!(
                "{}       {} ms       {} bytes",
                row.status, row.duration_ms, row.response_bytes
            )
        })
        .truncate(),
    );
    if let Some(error) = &row.error {
        ui.colored_label(Color32::LIGHT_RED, error);
    }
    ui.separator();
    if panel.tab == MessageTab::Overview {
        for (key, value) in [
            ("方法", row.method.clone()),
            ("协议", row.protocol.clone()),
            ("应用程序", row.application.clone()),
            ("服务器 IP", row.server_ip.clone()),
            ("状态", row.status.to_string()),
            ("URL", row.url.clone()),
        ] {
            ui.horizontal(|ui| {
                ui.add_sized([85.0, 20.0], egui::Label::new(RichText::new(key).weak()));
                ui.add(egui::Label::new(value).wrap());
            });
        }
        return;
    }
    let Some(viewer) = &panel.viewer else {
        ui.label("正在读取归档…");
        return;
    };
    if panel.tab == MessageTab::Headers {
        egui::ScrollArea::both()
            .id_salt(("headers", index))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new(("header_grid", index))
                    .striped(true)
                    .show(ui, |ui| {
                        for line in viewer.detail.headers.lines() {
                            let (name, value) = line.split_once(':').unwrap_or((line, ""));
                            ui.label(RichText::new(name).color(ACCENT));
                            ui.label(value.trim());
                            ui.end_row();
                        }
                        for line in viewer.detail.trailers.lines() {
                            ui.weak("Trailer");
                            ui.monospace(line);
                            ui.end_row();
                        }
                    });
            });
        return;
    }
    ui.horizontal(|ui| {
        ui.add_enabled_ui(matches!(viewer.content, Content::Text { .. }), |ui| {
            ui.selectable_value(&mut panel.hex, false, "Raw");
        });
        ui.selectable_value(&mut panel.hex, true, "Hex");
        if let Content::Text { text, .. } = &viewer.content {
            if ui
                .small_button(if viewer.detail.notice.is_none() {
                    "复制完整正文"
                } else {
                    "复制预览"
                })
                .clicked()
            {
                ui.ctx().copy_text(text.clone());
            }
        }
    });
    if let Some(notice) = &viewer.detail.notice {
        ui.colored_label(Color32::YELLOW, notice);
    }
    let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
    let count = if panel.hex {
        viewer.content.bytes().len().div_ceil(16)
    } else {
        match &viewer.content {
            Content::Text { index, .. } => index.count,
            Content::Binary(_) => 0,
        }
    };
    if panel.tab == MessageTab::Raw {
        egui::ScrollArea::both()
            .id_salt(("raw_headers", index))
            .max_height(70.0)
            .show(ui, |ui| {
                ui.monospace(&viewer.detail.headers);
            });
    }
    egui::ScrollArea::both()
        .id_salt(("body", index, &viewer.detail.id, panel.hex))
        .auto_shrink([false, false])
        .show_rows(ui, row_height, count, |ui, range| {
            if panel.hex {
                for row in range {
                    ui.monospace(hex_row(viewer.content.bytes(), row));
                }
            } else if let Content::Text { text, index } = &viewer.content {
                for line in index.rows(text, range) {
                    let line = line.strip_suffix('\n').unwrap_or(line);
                    let line = line.strip_suffix('\r').unwrap_or(line);
                    ui.add(
                        egui::Label::new(
                            RichText::new(if line.is_empty() { " " } else { line }).monospace(),
                        )
                        .extend(),
                    );
                }
            }
        });
}

fn dialogs(app: &mut Recho, ctx: &egui::Context) {
    if app.connection_open {
        let mut open = true;
        egui::Window::new("连接设置")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(480.0)
            .show(ctx, |ui| {
                ui.label(format!("代理地址  127.0.0.1:{}", app.port));
                ui.horizontal(|ui| {
                    ui.label("监听端口");
                    ui.text_edit_singleline(&mut app.port_editor);
                    if ui.button("应用端口").clicked() {
                        match app.port_editor.parse::<u16>() {
                            Ok(port) => app.send(Command::Port(port)),
                            Err(_) => app.error = Some("端口必须是 0–65535".into()),
                        }
                    }
                });
                let mut intercept = app.intercept_tls;
                if ui.checkbox(&mut intercept, "HTTPS 解密").changed() {
                    app.send(Command::Configure {
                        running: app.running,
                        intercept_tls: intercept,
                    });
                }
                ui.separator();
                ui.label("客户端需要手动配置代理。解密 HTTPS 时，请在客户端显式信任以下本地 CA：");
                ui.label(app.ca_path.display().to_string());
                if ui.button("复制 CA 路径").clicked() {
                    ctx.copy_text(app.ca_path.display().to_string());
                }
                if ui.button("复制代理地址").clicked() {
                    ctx.copy_text(format!("127.0.0.1:{}", app.port));
                }
                ui.weak("不会自动修改系统代理或证书信任。");
            });
        app.connection_open = open;
    }
    if app.clear_confirmation {
        let mut open = true;
        egui::Window::new("清空捕获")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("清空此捕获中的记录？持久化历史将保留。");
                ui.horizontal(|ui| {
                    if ui.button("清空").clicked() {
                        app.select(None);
                        app.send(Command::Clear);
                        app.clear_confirmation = false;
                    }
                    if ui.button("取消").clicked() {
                        app.clear_confirmation = false;
                    }
                });
            });
        if !open {
            app.clear_confirmation = false;
        }
    }
}
