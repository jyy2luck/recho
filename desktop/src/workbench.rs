use super::*;
use crate::certificate_ui::Page as CertificatePage;
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
    for (key, navigation) in [
        (egui::Key::F1, Navigation::Capture),
        (egui::Key::F2, Navigation::Collections),
        (egui::Key::F3, Navigation::Environments),
        (egui::Key::F4, Navigation::History),
    ] {
        if ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, key)) {
            app.navigate(navigation);
            if navigation == Navigation::Capture {
                app.composer.show_capture();
            }
        }
    }
    if ui.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::N)) && app.ready
    {
        app.composer.open_new();
        if app.navigation == Navigation::History {
            app.navigate(Navigation::Capture);
        }
    }
    #[cfg(target_os = "macos")]
    crate::native_menu::show(app, ui);
    let colors = app.theme.palette();
    #[cfg(not(target_os = "macos"))]
    egui::Panel::top("menus")
        .frame(
            egui::Frame::new()
                .fill(colors.chrome)
                .inner_margin(egui::Margin::symmetric(8, 1)),
        )
        .show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.label(RichText::new("recho").strong().color(colors.accent));
                ui.menu_button("文件", |ui| {
                    if ui
                        .add_enabled(app.ready, egui::Button::new("新建 API 请求"))
                        .clicked()
                    {
                        app.composer.open_new();
                        if app.navigation == Navigation::History {
                            app.navigate(Navigation::Capture);
                        }
                        ui.close();
                    }
                    if ui
                        .add_enabled(app.ready, egui::Button::new("新建原始请求"))
                        .clicked()
                    {
                        app.composer.open_raw();
                        if app.navigation == Navigation::History {
                            app.navigate(Navigation::Capture);
                        }
                        ui.close();
                    }
                    if ui
                        .add_enabled(app.ready, egui::Button::new("导入 HAR"))
                        .clicked()
                    {
                        app.choose_har(ui.ctx(), false);
                        ui.close();
                    }
                    if ui
                        .add_enabled(!app.selection.is_empty(), egui::Button::new("导出所选 HAR"))
                        .clicked()
                    {
                        app.choose_har(ui.ctx(), true);
                        ui.close();
                    }
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
                    ui.menu_button("主题", |ui| {
                        for theme in theme::Theme::ALL {
                            if ui
                                .selectable_label(app.theme == theme, theme.name())
                                .clicked()
                            {
                                app.set_theme(theme, ui.ctx());
                                ui.close();
                            }
                        }
                    });
                });
                ui.menu_button("工具", |ui| {
                    if ui.button("MCP").clicked() {
                        app.mcp.open();
                        ui.close();
                    }
                    if ui.button("访问令牌").clicked() {
                        app.mcp.open_tokens();
                        ui.close();
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
                    let mut incognito = app.spaces.incognito;
                    if ui
                        .add_enabled(
                            app.ready && app.spaces.settings.is_none(),
                            egui::Checkbox::new(&mut incognito, "无痕模式"),
                        )
                        .changed()
                    {
                        app.set_incognito(incognito);
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
                    let mut enabled = app.system_proxy.0;
                    if ui
                        .add_enabled(app.ready, egui::Checkbox::new(&mut enabled, "系统代理"))
                        .changed()
                    {
                        app.send(Command::SystemProxy(enabled));
                    }
                    if app.system_proxy.1
                        && !app.system_proxy.0
                        && ui.button("恢复系统代理").clicked()
                    {
                        app.send(Command::SystemProxy(false));
                    }
                    ui.separator();
                    proxy_preferences(app, ui);
                });
                ui.menu_button("证书", |ui| {
                    let mut intercept = app.intercept_tls;
                    if ui.checkbox(&mut intercept, "HTTPS 解密").changed() {
                        app.send(Command::Configure {
                            running: app.running,
                            intercept_tls: intercept,
                        });
                    }
                    ui.separator();
                    for (page, label) in CertificatePage::MENU {
                        if ui
                            .add_enabled(app.ready, egui::Button::new(label))
                            .clicked()
                        {
                            app.certificates.open(page);
                            ui.close();
                        }
                    }
                    ui.menu_button("导出 CA 证书", |ui| {
                        for (format, label) in CertificatePage::EXPORTS {
                            if ui
                                .add_enabled(app.ready, egui::Button::new(label))
                                .clicked()
                            {
                                app.certificates.export(ui.ctx(), format);
                                ui.close();
                            }
                        }
                    });
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
                .fill(colors.chrome)
                .inner_margin(egui::Margin::symmetric(8, 3)),
        )
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if icon_button(ui, "\u{e8f4}", "展开或收起侧边栏", true).clicked() {
                    app.sidebar_visible = !app.sidebar_visible;
                }
                if app.rules.pending_count() > 0 {
                    ui.colored_label(colors.accent, format!("断点 {}", app.rules.pending_count()));
                }
                if icon_button(ui, "\u{e869}", "调试工具", true)
                    .on_hover_text("调试工具")
                    .clicked()
                {
                    app.navigate(Navigation::Tools);
                }
                ui.label(format!(
                    "共 {} 项 · 已选择 {}",
                    if app.navigation == Navigation::History {
                        app.history_rows.len()
                    } else {
                        app.visible.len()
                    },
                    app.selection.len()
                ));
                if let Some(notice) = &app.notice {
                    ui.label(notice);
                    if ui.small_button("×").clicked() {
                        app.notice = None;
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak("0.1.0 · 本地");
                });
            });
            for error in [&app.history_error, &app.error, &app.spaces.error]
                .into_iter()
                .flatten()
            {
                ui.colored_label(colors.red, error);
            }
            if app.error.is_some() && ui.small_button("关闭提示").clicked() {
                app.error = None;
            }
        });
    egui::Panel::left("rail")
        .exact_size(40.0)
        .frame(egui::Frame::new().fill(colors.chrome).inner_margin(4))
        .show(ui, |ui| {
            for (icon, title, page) in [
                ("\u{e871}", "工作台 (F1)", Navigation::Capture),
                ("\u{e53b}", "集合 (F2)", Navigation::Collections),
                ("\u{e24a}", "环境变量 (F3)", Navigation::Environments),
                ("\u{e889}", "历史 (F4)", Navigation::History),
                ("\u{e869}", "调试工具", Navigation::Tools),
            ] {
                let response = ui.add(
                    egui::Button::selectable(app.navigation == page, self::icon(icon))
                        .min_size(egui::vec2(30.0, 34.0)),
                );
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, true, title)
                });
                if response.on_hover_text(title).clicked() {
                    app.navigate(page);
                    if page == Navigation::Capture {
                        app.composer.show_capture();
                    }
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
                    .fill(colors.side)
                    .inner_margin(egui::Margin::symmetric(10, 7)),
            )
            .show(ui, |ui| sidebar(app, ui));
    }
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(colors.bg))
        .show(ui, |ui| {
            proxy_bar(app, ui);
            egui::ScrollArea::horizontal()
                .id_salt("tabs")
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        capture_tabs(app, ui);
                        app.composer.show_tabs(ui, &app.worker);
                        ui.add_enabled_ui(app.ready, |ui| {
                            ui.menu_button("＋", |ui| {
                                if ui.button("新建调试会话").clicked() {
                                    app.navigate(Navigation::Capture);
                                    app.composer.show_capture();
                                    app.new_capture_space();
                                    ui.close();
                                }
                                if ui.button("导入 HAR").clicked() {
                                    app.choose_har(ui.ctx(), false);
                                    ui.close();
                                }
                                if ui.button("HTTP 请求").clicked() {
                                    if app.navigation == Navigation::History {
                                        app.navigate(Navigation::Capture);
                                    }
                                    app.composer.open_new();
                                    ui.close();
                                }
                                if ui.button("原始 HTTP 请求").clicked() {
                                    if app.navigation == Navigation::History {
                                        app.navigate(Navigation::Capture);
                                    }
                                    app.composer.open_raw();
                                    ui.close();
                                }
                                if ui.button("WebSocket 请求").clicked() {
                                    app.composer.open_websocket();
                                    ui.close();
                                }
                                if ui.button("gRPC 请求").clicked() {
                                    app.composer.open_grpc();
                                    ui.close();
                                }
                                if ui.button("Protobuf 规范").clicked() {
                                    app.composer.open_protobuf();
                                    ui.close();
                                }
                            });
                        });
                    })
                });
            ui.separator();
            if app.library.is_open() {
                if let Some(captured) = app.library.show(ui, &app.worker) {
                    app.library.close();
                    app.composer.open_library_capture(captured);
                }
                return;
            }
            if app.navigation == Navigation::Tools && app.network_open {
                if ui.small_button("返回抓包").clicked() {
                    app.network_open = false;
                }
                app.network.show(ui, &app.worker);
                return;
            }
            if app.navigation == Navigation::Tools && app.rules_open {
                if ui.small_button("返回抓包").clicked() {
                    app.rules_open = false;
                }
                app.rules.show(ui, &app.worker);
                return;
            }
            if app.composer.active() {
                if app.navigation == Navigation::History {
                    app.navigate(Navigation::Capture);
                }
                app.composer.show(ui, &app.worker, app.port);
                return;
            }
            if app.navigation == Navigation::History {
                history(app, ui);
                return;
            }
            filters(app, ui);
            ui.horizontal(|ui| {
                if ui.button("条件筛选").clicked() {
                    app.condition_editor = app
                        .spaces
                        .current()
                        .map(|space| (false, space.conditions.clone()));
                }
                if !app.capture_filters.is_empty() {
                    ui.weak(format!("MCP 筛选 ({})", app.capture_filters.len()));
                }
                ui.label(icon("\u{e8b6}"));
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut app.filter)
                            .hint_text("搜索 URL、方法、状态")
                            .desired_width((ui.available_width() - 46.0).max(80.0)),
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
    app.composer.show_dialogs(ui.ctx(), &app.worker);
    app.rules.dialogs(ui.ctx(), &app.worker);
    app.mcp.show(ui.ctx(), &app.worker, app.port, app.ready);
}

fn sidebar(app: &mut Recho, ui: &mut egui::Ui) {
    match app.navigation {
        Navigation::Collections | Navigation::Environments => {
            app.composer
                .show_sidebar(ui, &app.worker, app.navigation == Navigation::Environments);
            return;
        }
        Navigation::History => {
            ui.strong("历史");
            ui.weak("按最近捕获时间浏览本地记录");
            return;
        }
        _ => {}
    }
    ui.weak(if app.navigation == Navigation::Tools {
        "调试工具"
    } else {
        "工作台"
    });
    ui.separator();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if app.navigation == Navigation::Tools {
                section(ui, "调试", |ui| {
                    if ui.button("自动高亮").clicked() {
                        app.condition_editor = app
                            .spaces
                            .current()
                            .map(|space| (true, space.highlights.clone()));
                    }
                    use crate::rules_ui::RuleCategory;
                    for (label, category) in [
                        ("全部规则", RuleCategory::All),
                        ("重写", RuleCategory::Rewrite),
                        ("断点", RuleCategory::Breakpoint),
                        ("脚本", RuleCategory::Script),
                    ] {
                        if ui.button(label).clicked() {
                            app.rules.open(category);
                            app.rules_open = true;
                            app.network_open = false;
                        }
                    }
                });
                section(ui, "代理", |ui| {
                    use crate::network_ui::NetworkSection;
                    for (label, section) in [
                        ("SSL 配置", NetworkSection::Ssl),
                        ("访问控制", NetworkSection::Access),
                        ("二级代理", NetworkSection::Upstream),
                        ("网关", NetworkSection::Gateway),
                        ("镜像", NetworkSection::Mirror),
                        ("反向代理", NetworkSection::Reverse),
                    ] {
                        if ui.button(label).clicked() {
                            app.open_network(section);
                        }
                    }
                });
                section(ui, "本地工具", |ui| {
                    if ui.button("MCP").clicked() {
                        app.mcp.open();
                    }
                    if ui.button("访问令牌").clicked() {
                        app.mcp.open_tokens();
                    }
                    if ui.button("安装 CA 证书").clicked() {
                        app.certificates.open(CertificatePage::Local);
                    }
                });
            } else {
                section(ui, "规范文件", |ui| {
                    app.composer.show_specifications(ui);
                });
                app.library.show_sidebar(ui, &app.worker);
                section(ui, "设备", |ui| {
                    if ui.button("安装 CA 证书").clicked() {
                        app.certificates.open(CertificatePage::Local);
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
    let colors = app.theme.palette();
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
                    .fill(colors.chrome)
                    .stroke(egui::Stroke::new(1.0, colors.line))
                    .corner_radius(4)
                    .inner_margin(8)
                    .show(ui, |ui| {
                        ui.set_width((width - 16.0).max(120.0));
                        ui.horizontal(|ui| {
                            if ui
                                .add(
                                    egui::Button::new(RichText::new(text).color(if app.running {
                                        colors.green
                                    } else {
                                        colors.muted
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
                                    if icon_button(ui, "\u{e9e0}", "安装 CA 证书", true).clicked()
                                    {
                                        app.certificates.open(CertificatePage::Local);
                                    }
                                    let (active, pending) = app.system_proxy;
                                    let title = if active || pending {
                                        "恢复系统代理"
                                    } else {
                                        "启用系统代理"
                                    };
                                    if icon_button(
                                        ui,
                                        "\u{e894}",
                                        title,
                                        app.ready && (app.running || pending),
                                    )
                                    .clicked()
                                    {
                                        app.send(Command::SystemProxy(!active && !pending));
                                    }
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

#[cfg(not(target_os = "macos"))]
fn copy_url(app: &Recho, ui: &egui::Ui) {
    if let Some(row) = app
        .active_rows()
        .iter()
        .find(|row| app.selected.as_ref() == Some(&row.id))
    {
        ui.ctx().copy_text(row.url.clone());
    }
}
fn capture_tabs(app: &mut Recho, ui: &mut egui::Ui) {
    let colors = app.theme.palette();
    if app.spaces.items.is_empty() {
        ui.weak("调试");
        return;
    }
    let mut next = None;
    let mut remove = None;
    for (id, space) in &mut app.spaces.items {
        let current = *id == app.spaces.group;
        let label = if current {
            format!("◉  {} ({})", space.name, app.rows.len())
        } else {
            space.name.clone()
        };
        let response = ui.add(
            egui::Button::selectable(
                current && !app.composer.active() && app.navigation != Navigation::History,
                RichText::new(label).color(if current {
                    colors.accent
                } else {
                    ui.visuals().text_color()
                }),
            )
            .min_size(egui::vec2(100.0, 30.0)),
        );
        response.context_menu(|ui| {
            ui.label("会话名称");
            ui.add(egui::TextEdit::singleline(&mut space.name).char_limit(80));
            if ui
                .add_enabled(
                    !current && id != "default",
                    egui::Button::new("关闭会话标签"),
                )
                .clicked()
            {
                remove = Some(id.clone());
                ui.close();
            }
        });
        if response.clicked() {
            next = Some(id.clone());
        }
    }
    if let Some(id) = remove {
        app.spaces.items.remove(&id);
    }
    if let Some(id) = next {
        app.navigate(Navigation::Capture);
        app.composer.show_capture();
        app.activate_capture(id);
    }
}

fn history(app: &mut Recho, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.strong("历史");
        if ui
            .add(egui::TextEdit::singleline(&mut app.history_query).hint_text("搜索历史"))
            .changed()
        {
            app.load_history(0);
        }
        if ui.button("刷新").clicked() {
            app.load_history(app.history_offset);
        }
        if ui
            .add_enabled(app.history_offset > 0, egui::Button::new("上一页"))
            .clicked()
        {
            app.load_history(app.history_offset.saturating_sub(100));
        }
        if ui
            .add_enabled(app.history_rows.len() == 100, egui::Button::new("下一页"))
            .clicked()
        {
            app.load_history(app.history_offset + 100);
        }
        ui.weak(format!("第 {} 页", app.history_offset / 100 + 1));
    });
    if app.history_load.is_some() {
        ui.spinner();
    } else if app.history_rows.is_empty() {
        ui.label(if app.history_loaded {
            "没有匹配的历史记录"
        } else {
            "历史尚未加载"
        });
        return;
    }
    ui.separator();
    if app.details_open && app.selected.is_some() {
        egui::Panel::right("history_detail")
            .default_size((ui.available_width() * 0.5).max(360.0))
            .min_size(330.0)
            .resizable(true)
            .show(ui, |ui| details(app, ui));
    }
    capture_table(app, ui);
}

fn capture_table(app: &mut Recho, ui: &mut egui::Ui) {
    let colors = app.theme.palette();
    let history = app.navigation == Navigation::History;
    if history && app.history_rows.is_empty() {
        return;
    }
    if !history && app.rows.is_empty() {
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
    let focus_id = ui.make_persistent_id(("capture_focus", history));
    ui.interact(
        egui::Rect::NOTHING,
        focus_id,
        egui::Sense::focusable_noninteractive(),
    );
    ui.memory_mut(|memory| {
        memory.set_focus_lock_filter(
            focus_id,
            egui::EventFilter {
                vertical_arrows: true,
                ..Default::default()
            },
        )
    });
    let mut scroll_to = None;
    if ui.memory(|memory| memory.has_focus(focus_id)) {
        let movement = ui.input_mut(|input| {
            input.count_and_consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) as isize
                - input.count_and_consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) as isize
        });
        if movement != 0 {
            let ids = visible_rows(app, history);
            let current = app
                .selected
                .as_ref()
                .and_then(|id| ids.iter().position(|row| &row.id == id));
            if let Some(index) = moved_index(ids.len(), current, movement) {
                let id = ids[index].id.clone();
                app.select(Some(id));
                scroll_to = Some(index);
            }
        }
        if ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            app.details_open = false;
        }
        if ui.input_mut(|input| {
            input.consume_key(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::C,
            )
        }) {
            if let Some(id) = app.selected.clone() {
                app.copy_curl(id);
            }
        }
    }
    let mut choose = None;
    let mut open = false;
    let mut sort = None;
    let mut curl = None;
    let mut edit = None;
    let mut hide = false;
    let mut export = false;
    let mut keep = None;
    let mut rule = None;
    let modifiers = ui.input(|input| input.modifiers);
    let rows = visible_rows(app, history);
    egui::ScrollArea::horizontal()
        .id_salt(("table_horizontal", history))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_min_width(980.0);
            ui.visuals_mut().widgets.hovered.bg_fill = colors.chrome;
            let table = TableBuilder::new(ui)
                .id_salt(("captures", history))
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
            let table = if let Some(row) = scroll_to {
                table.scroll_to_row(row, None)
            } else {
                table
            };
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
                                    !history && app.sort_column == index,
                                    format!(
                                        "{title}{}",
                                        if !history && app.sort_column == index {
                                            if app.ascending { " ^" } else { " v" }
                                        } else {
                                            ""
                                        }
                                    ),
                                )
                                .clicked()
                            {
                                if !history {
                                    sort = Some(index);
                                }
                            }
                        });
                    }
                })
                .body(|body| {
                    body.rows(26.0, rows.len(), |mut table_row| {
                        let row = rows[table_row.index()];
                        table_row.set_selected(app.selection.contains(&row.id));
                        let color = if row.error.is_some() || row.status >= 400 {
                            colors.red
                        } else {
                            colors.green
                        };
                        let cells = [
                            if history {
                                (app.history_offset + table_row.index() + 1).to_string()
                            } else {
                                app.sequence.get(&row.id).unwrap_or(&0).to_string()
                            },
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
                                        } else if !history && app.spaces.marked.contains(&row.id) {
                                            colors.accent
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
                        if response.clicked() {
                            choose = Some((row.id.clone(), modifiers));
                        }
                        if response.secondary_clicked() && !app.selection.contains(&row.id) {
                            choose = Some((row.id.clone(), egui::Modifiers::NONE));
                        }
                        if response.double_clicked() {
                            choose = Some((row.id.clone(), egui::Modifiers::NONE));
                            open = true;
                        }
                        response.context_menu(|ui| {
                            if ui.button("复制 cURL").clicked() {
                                curl = Some(row.id.clone());
                                ui.close();
                            }
                            if ui.button("编辑并重发").clicked() {
                                edit = Some(row.id.clone());
                                ui.close();
                            }
                            if ui.button("查看详情").clicked() {
                                choose = Some((row.id.clone(), egui::Modifiers::NONE));
                                open = true;
                                ui.close();
                            }
                            if ui.button("复制 URL").clicked() {
                                ui.ctx().copy_text(row.url.clone());
                                ui.close();
                            }
                            for (caption, category) in [
                                ("添加重写规则", crate::rules_ui::RuleCategory::Rewrite),
                                ("添加断点", crate::rules_ui::RuleCategory::Breakpoint),
                                ("添加脚本", crate::rules_ui::RuleCategory::Script),
                            ] {
                                if ui.button(caption).clicked() {
                                    rule = Some((category, row.method.clone(), row.url.clone()));
                                    ui.close();
                                }
                            }
                            if !history && ui.button("从此会话移除所选").clicked() {
                                hide = true;
                                ui.close();
                            }
                            if ui.button("导出所选 HAR").clicked() {
                                export = true;
                                ui.close();
                            }
                            for (caption, compare, now) in [
                                ("添加书签", false, false),
                                ("添加到对比池", true, false),
                                ("对比所选", true, true),
                            ] {
                                if ui
                                    .add_enabled(
                                        !row.incognito && (!now || app.selection.len() >= 2),
                                        egui::Button::new(caption),
                                    )
                                    .clicked()
                                {
                                    keep = Some((compare, now));
                                    ui.close();
                                }
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
    if let Some((id, modifiers)) = choose {
        app.select_row(id, modifiers);
        ui.memory_mut(|memory| memory.request_focus(focus_id));
    }
    if let Some(id) = curl {
        app.copy_curl(id);
    }
    if let Some(id) = edit {
        app.edit_capture(&id);
    }
    if hide {
        app.hide_selection();
    }
    if export {
        app.choose_har(ui.ctx(), true);
    }
    if let Some((compare, now)) = keep {
        let rows = if history {
            &app.history_rows
        } else {
            &app.rows
        };
        let rows = rows
            .iter()
            .filter(|row| app.selection.contains(&row.id))
            .collect::<Vec<_>>();
        app.library.add(&app.worker, &rows, compare, now);
    }
    if let Some((category, method, url)) = rule {
        app.navigate(Navigation::Tools);
        app.rules_open = true;
        app.network_open = false;
        app.rules.new_for_capture(category, &method, &url);
    }
}

fn visible_rows(app: &Recho, history: bool) -> Vec<&Row> {
    if history {
        app.history_rows.iter().collect()
    } else {
        app.visible.iter().map(|&index| &app.rows[index]).collect()
    }
}

fn moved_index(len: usize, current: Option<usize>, step: isize) -> Option<usize> {
    let end = len.checked_sub(1)?;
    Some(match current {
        Some(index) => index.saturating_add_signed(step).min(end),
        None if step < 0 => end.saturating_sub(step.unsigned_abs() - 1),
        None => (step as usize - 1).min(end),
    })
}

#[cfg(test)]
mod selection_tests {
    use super::moved_index;

    #[test]
    fn capture_navigation_does_not_wrap_and_can_start_without_selection() {
        assert_eq!(moved_index(0, None, -1), None);
        assert_eq!(moved_index(4, None, -1), Some(3));
        assert_eq!(moved_index(4, None, 1), Some(0));
        assert_eq!(moved_index(4, Some(0), -1), Some(0));
        assert_eq!(moved_index(4, Some(3), 1), Some(3));
        assert_eq!(moved_index(80, Some(15), 45), Some(60));
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
    let colors = app.theme.palette();
    let request = index == 0;
    let rows = if app.navigation == Navigation::History {
        &app.history_rows
    } else {
        &app.rows
    };
    let Some(row) = rows
        .iter()
        .find(|row| app.selected.as_ref() == Some(&row.id))
    else {
        return;
    };
    let panel = &mut app.messages[index];
    ui.horizontal(|ui| {
        ui.label(RichText::new(if request { "请求" } else { "响应" }).color(colors.accent));
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
        ui.colored_label(colors.red, error);
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
    let Some(viewer) = &mut panel.viewer else {
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
                            ui.label(RichText::new(name).color(colors.key));
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
            ui.selectable_value(&mut panel.format, BodyFormat::Raw, "Raw");
            ui.selectable_value(&mut panel.format, BodyFormat::Json, "JSON");
            ui.selectable_value(&mut panel.format, BodyFormat::Tree, "Tree");
        });
        ui.selectable_value(&mut panel.format, BodyFormat::Hex, "Hex");
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
        ui.colored_label(colors.warning, notice);
    }
    if matches!(panel.format, BodyFormat::Json | BodyFormat::Tree) {
        let parsed = viewer.structured.get_or_insert_with(|| {
            let bytes = viewer.content.bytes();
            if bytes.len() > 8 * 1024 * 1024 {
                return Err("结构化视图支持最多 8 MiB，完整正文可在 Raw / Hex 查看".into());
            }
            serde_json::from_slice(bytes).map_err(|error| format!("正文不是有效 JSON：{error}"))
        });
        match parsed {
            Ok(value) => {
                ui.push_id((&viewer.detail.id, index), |ui| {
                    body::show_structured(ui, value, panel.format == BodyFormat::Tree);
                });
            }
            Err(error) => {
                ui.label(error.as_str());
            }
        }
        return;
    }
    let hex = panel.format == BodyFormat::Hex;
    let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
    let count = if hex {
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
        .id_salt(("body", index, &viewer.detail.id, hex))
        .auto_shrink([false, false])
        .show_rows(ui, row_height, count, |ui, range| {
            if hex {
                for row in range {
                    ui.add(
                        egui::Label::new(
                            RichText::new(hex_row(viewer.content.bytes(), row)).monospace(),
                        )
                        .extend(),
                    );
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

fn condition_dialog(app: &mut Recho, ctx: &egui::Context) {
    use recho_core::native::{Condition, Field, Operator};
    let Some((highlight, conditions)) = &mut app.condition_editor else {
        return;
    };
    let mut opened = true;
    let mut save = false;
    egui::Window::new(if *highlight {
        "自动高亮"
    } else {
        "条件筛选"
    })
    .open(&mut opened)
    .default_width(660.0)
    .resizable(true)
    .show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut conditions.any, false, "满足全部");
            ui.selectable_value(&mut conditions.any, true, "满足任一");
        });
        let mut remove = None;
        egui::ScrollArea::vertical()
            .max_height(360.0)
            .show(ui, |ui| {
                for (index, item) in conditions.items.iter_mut().enumerate() {
                    ui.push_id(index, |ui| {
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut item.enabled, "");
                            egui::ComboBox::from_id_salt("field")
                                .selected_text(item.field.label())
                                .show_ui(ui, |ui| {
                                    for field in Field::ALL {
                                        ui.selectable_value(&mut item.field, field, field.label());
                                    }
                                });
                            egui::ComboBox::from_id_salt("operator")
                                .selected_text(item.operator.label())
                                .show_ui(ui, |ui| {
                                    for operator in Operator::ALL {
                                        ui.selectable_value(
                                            &mut item.operator,
                                            operator,
                                            operator.label(),
                                        );
                                    }
                                });
                            ui.add(
                                egui::TextEdit::singleline(&mut item.value)
                                    .hint_text("匹配值")
                                    .char_limit(4096)
                                    .desired_width(175.0),
                            );
                            ui.checkbox(&mut item.sensitive, "Aa")
                                .on_hover_text("区分大小写");
                            if item.field == Field::All {
                                ui.checkbox(&mut item.deep, "深度");
                            }
                            if ui.small_button("移除").clicked() {
                                remove = Some(index);
                            }
                        });
                    });
                }
            });
        if let Some(index) = remove {
            conditions.items.remove(index);
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(conditions.items.len() < 32, egui::Button::new("添加条件"))
                .clicked()
            {
                conditions.items.push(Condition::default());
            }
            let validation = conditions.validate();
            if ui
                .add_enabled(validation.is_ok(), egui::Button::new("应用"))
                .clicked()
            {
                save = true;
            }
            if let Err(error) = validation {
                ui.label(error);
            }
        });
        ui.weak("正文与控制台按抓包预览匹配。");
    });
    if save {
        if let Some((highlight, conditions)) = app.condition_editor.take() {
            if let Some(space) = app.spaces.items.get_mut(&app.spaces.group) {
                if highlight {
                    space.highlights = conditions;
                } else {
                    space.conditions = conditions;
                }
                app.evaluate_conditions();
            }
        }
    } else if !opened {
        app.condition_editor = None;
    }
}

fn dialogs(app: &mut Recho, ctx: &egui::Context) {
    condition_dialog(app, ctx);
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
                let (active, pending) = app.system_proxy;
                let label = if active || pending {
                    "恢复系统代理"
                } else {
                    "启用系统代理"
                };
                if ui
                    .add_enabled(
                        app.ready && (app.running || pending),
                        egui::Button::new(label),
                    )
                    .clicked()
                {
                    app.send(Command::SystemProxy(!pending && !active));
                }
                proxy_preferences(app, ui);
                ui.label("客户端可使用上方代理地址。解密 HTTPS 时需信任本地 CA。");
                if ui.button("安装 CA 证书").clicked() {
                    app.certificates.open(CertificatePage::Local);
                }
                if ui.button("复制代理地址").clicked() {
                    ctx.copy_text(format!("127.0.0.1:{}", app.port));
                }
            });
        app.connection_open = open;
    }
    app.certificates
        .show(ctx, app.theme.palette(), app.port, app.running, app.ready);
    if app.confirm_proxy_auto {
        let modal = egui::Modal::new(egui::Id::new("proxy_auto_confirmation")).show(ctx, |ui| {
            ui.set_width(400.0);
            ui.heading("随调试启动启用系统代理");
            ui.label("下次启动 recho 或恢复调试时，recho 会将选中的协议指向本机监听端口。暂停、退出或手动恢复时会恢复原设置。");
            ui.horizontal(|ui| (ui.button("启用").clicked(), ui.button("取消").clicked())).inner
        });
        if modal.inner.0 {
            app.set_proxy_preferences(platform::ProxyPreferences {
                auto: true,
                ..app.proxy_preferences
            });
            app.confirm_proxy_auto = false;
        } else if modal.inner.1 || modal.should_close() {
            app.confirm_proxy_auto = false;
        }
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
                        app.clear_capture_space();
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

fn proxy_preferences(app: &mut Recho, ui: &mut egui::Ui) {
    use platform::ProxyProtocol;
    ui.add_enabled_ui(app.ready && !app.proxy_preferences_saving, |ui| {
        ui.horizontal_wrapped(|ui| {
            for protocol in ProxyProtocol::ALL {
                let mut checked = protocol.enabled(app.proxy_preferences);
                if ui
                    .add_enabled(
                        protocol.supported(),
                        egui::Checkbox::new(&mut checked, protocol.label()),
                    )
                    .on_disabled_hover_text("Windows 系统代理使用 WinINet，不能保证 SOCKS5")
                    .on_hover_text("下次启用系统代理时生效")
                    .changed()
                {
                    app.set_proxy_preferences(protocol.toggled(app.proxy_preferences));
                }
            }
        });
        let mut automatic = app.proxy_preferences.auto;
        if ui
            .checkbox(&mut automatic, "随调试启动启用系统代理")
            .changed()
        {
            app.toggle_proxy_auto();
        }
    });
}
