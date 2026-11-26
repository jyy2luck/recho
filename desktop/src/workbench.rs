use super::*;
use crate::certificate_ui::Page as CertificatePage;
use crate::theme::{CONTROL_HEIGHT, Theme, control_row, dialog_footer, dialog_header};
use egui_extras::{Column, TableBuilder};

const NEW_HTTP: egui::KeyboardShortcut =
    egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::T);
const CLEAR_CAPTURE: egui::KeyboardShortcut =
    egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::X);

fn clear_capture_pressed(input: &mut egui::InputState) -> bool {
    if input.consume_shortcut(&CLEAR_CAPTURE) {
        return true;
    }
    // egui-winit translates Ctrl+X to a clipboard event on Windows and Linux.
    let ctrl_only = |modifiers: egui::Modifiers| {
        modifiers.ctrl && !modifiers.alt && !modifiers.shift && !modifiers.mac_cmd
    };
    let ctrl_x = ctrl_only(input.modifiers) || input.events.iter().any(|event| {
        matches!(event, egui::Event::Key { key: egui::Key::X, pressed: false, modifiers, .. } if ctrl_only(*modifiers))
    });
    if !ctrl_x {
        return false;
    }
    let Some(index) = input
        .events
        .iter()
        .position(|event| matches!(event, egui::Event::Cut))
    else {
        return false;
    };
    input.events.remove(index);
    true
}

#[cfg(not(target_os = "macos"))]
pub(super) const MENU_HEIGHT: f32 = if cfg!(windows) { 32.0 } else { 30.0 };
#[cfg(not(target_os = "macos"))]
const MENU_WIDTH: f32 = if cfg!(windows) { 48.0 } else { 46.0 };
#[cfg(windows)]
const MENU_FONT_SIZE: f32 = 15.0;
#[cfg(windows)]
const MENU_CONTENT_WIDTH: f32 = 138.0;

pub(super) fn icon(code: &str) -> RichText {
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

pub(super) fn sidebar_row(
    ui: &mut egui::Ui,
    title: impl Into<egui::WidgetText>,
    selected: Option<bool>,
) -> egui::Response {
    let title = title.into();
    let button = match selected {
        Some(selected) => egui::Button::selectable(selected, title),
        None => egui::Button::new(title).frame_when_inactive(false),
    };
    ui.add(
        button
            .right_text(())
            .min_size(egui::vec2(ui.available_width(), 0.0))
            .wrap_mode(egui::TextWrapMode::Truncate),
    )
}

fn action_menu(popup: egui::Popup<'_>, content: impl FnOnce(&mut egui::Ui)) {
    let ctx = popup.ctx();
    let colors = crate::theme::Theme::current_palette(ctx);
    let frame = egui::Frame::popup(&ctx.style_of(ctx.theme()))
        .inner_margin(5)
        .corner_radius(7)
        .fill(colors.side)
        .stroke(egui::Stroke::new(1.0, colors.line));
    popup.width(210.0).frame(frame).show(|ui| {
        ui.set_min_width(198.0);
        ui.spacing_mut().button_padding = egui::vec2(9.0, 4.0);
        ui.spacing_mut().interact_size.y = 26.0;
        ui.spacing_mut().item_spacing.y = 2.0;
        content(ui);
    });
}

fn menu_item(ui: &mut egui::Ui, label: &str, enabled: bool) -> bool {
    ui.add_enabled_ui(enabled, |ui| sidebar_row(ui, label, None))
        .inner
        .clicked()
}

#[cfg(windows)]
fn top_menu_style(style: &mut egui::Style) {
    egui::containers::menu::menu_style(style);
    style.override_font_id = Some(egui::FontId::proportional(MENU_FONT_SIZE));
    style.wrap_mode = Some(egui::TextWrapMode::Extend);
    style.spacing.interact_size.y = 30.0;
    style.spacing.button_padding = egui::vec2(10.0, 0.0);
    style.spacing.item_spacing.y = 0.0;
}

#[cfg(not(target_os = "macos"))]
fn show_menu_titles<const N: usize>(
    ui: &mut egui::Ui,
    titles: [&str; N],
    mut show_contents: impl FnMut(usize, &mut egui::Ui),
) -> f32 {
    ui.spacing_mut().item_spacing.x = 0.0;
    let ids: [egui::Id; N] = std::array::from_fn(|index| ui.make_persistent_id(("menu", index)));
    let active = ids
        .iter()
        .position(|&id| egui::Popup::is_id_open(ui.ctx(), id));
    let buttons: [egui::Response; N] = std::array::from_fn(|index| {
        let title = RichText::new(titles[index]);
        #[cfg(windows)]
        let title = title.size(MENU_FONT_SIZE);
        ui.add(
            egui::Button::new(title)
                .min_size(egui::vec2(MENU_WIDTH, MENU_HEIGHT))
                .selected(active == Some(index))
                .frame_when_inactive(false)
                .stroke(egui::Stroke::NONE),
        )
    });
    let next = match buttons.iter().position(egui::Response::clicked) {
        Some(index) => (active != Some(index)).then_some(index),
        None => active.map(|index| {
            buttons
                .iter()
                .position(egui::Response::contains_pointer)
                .unwrap_or(index)
        }),
    };
    if next != active {
        if let Some(index) = active {
            egui::Popup::close_id(ui.ctx(), ids[index]);
        }
        if let Some(index) = next {
            egui::Popup::open_id(ui.ctx(), ids[index]);
        }
    }
    if let Some(index) = next {
        let popup = egui::Popup::menu(&buttons[index])
            .id(ids[index])
            .open_memory(None);
        #[cfg(windows)]
        let popup = popup.style(top_menu_style);
        popup.show(|ui| {
            #[cfg(windows)]
            ui.set_min_width(MENU_CONTENT_WIDTH);
            show_contents(index, ui);
        });
    }
    ui.cursor().min.x
}

#[cfg(all(test, not(target_os = "macos")))]
#[test]
fn top_menus_switch_on_hover_after_a_click() {
    for scale in [1.0, 1.25, 1.5] {
        check_top_menus_at_scale(scale);
    }
}

#[cfg(all(test, not(target_os = "macos")))]
fn check_top_menus_at_scale(scale: f32) {
    let ctx = egui::Context::default();
    let frame = |events| {
        let mut shown = None;
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(640.0, 240.0),
            )),
            events,
            ..Default::default()
        };
        input
            .viewports
            .get_mut(&input.viewport_id)
            .unwrap()
            .native_pixels_per_point = Some(scale);
        let mut output = ctx.run_ui(input, |ui| {
            shown = None;
            assert_eq!(ui.ctx().pixels_per_point(), scale);
            ui.horizontal(|ui| {
                show_menu_titles(ui, ["A", "B", "C"], |index, ui| {
                    shown = Some(index);
                    let short = ui.button("MCP");
                    let long = ui.button("Install the CA certificate on this computer");
                    #[cfg(windows)]
                    {
                        assert_eq!(
                            ui.style().override_font_id,
                            Some(egui::FontId::proportional(MENU_FONT_SIZE))
                        );
                        let pixel = 1.0 / scale;
                        assert!(short.rect.height() + pixel >= 30.0);
                        if !ui.is_sizing_pass() {
                            assert!(short.rect.width() + pixel >= MENU_CONTENT_WIDTH);
                        }
                        assert!(long.rect.width() > MENU_CONTENT_WIDTH);
                    }
                    let _ = (short, long);
                });
            });
        });
        output.textures_delta.clear();
        shown
    };
    let middle = egui::pos2(MENU_WIDTH * 1.5, MENU_HEIGHT / 2.0);
    let click = |pressed| egui::Event::PointerButton {
        pos: middle,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    assert_eq!(frame(vec![]), None);
    assert_eq!(frame(vec![egui::Event::PointerMoved(middle)]), None);
    assert_eq!(frame(vec![click(true)]), None);
    assert_eq!(frame(vec![click(false)]), Some(1));
    assert_eq!(frame(vec![]), Some(1));
    assert_eq!(
        frame(vec![egui::Event::PointerMoved(egui::pos2(
            MENU_WIDTH * 0.5,
            middle.y,
        ))]),
        Some(0)
    );
    assert_eq!(
        frame(vec![egui::Event::PointerMoved(egui::pos2(
            MENU_WIDTH * 2.5,
            middle.y,
        ))]),
        Some(2)
    );
    frame(vec![egui::Event::Key {
        key: egui::Key::Escape,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    }]);
    assert_eq!(frame(vec![]), None);
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
    if ui.input_mut(|input| {
        input.consume_key(egui::Modifiers::COMMAND, egui::Key::N)
            || (!cfg!(target_os = "macos") && input.consume_shortcut(&NEW_HTTP))
    }) && app.ready
    {
        app.composer.open_new();
        if app.navigation == Navigation::History {
            app.navigate(Navigation::Capture);
        }
    }
    #[cfg(target_os = "macos")]
    crate::native_menu::show(app, ui);
    let colors = app.theme.palette(ui.ctx());
    #[cfg(not(target_os = "macos"))]
    let menus = egui::Panel::top("menus");
    #[cfg(windows)]
    let menus = menus.exact_size(crate::title_bar::HEIGHT);
    #[cfg(not(target_os = "macos"))]
    menus
        .frame(
            egui::Frame::new()
                .fill(colors.chrome)
                .inner_margin(if cfg!(windows) {
                    egui::Margin::ZERO
                } else {
                    egui::Margin::symmetric(8, 1)
                }),
        )
        .show(ui, |ui| {
            #[cfg(windows)]
            let row = ui.available_rect_before_wrap();
            #[cfg(windows)]
            let mut menu_ui = crate::title_bar::TitleBar::menu_ui(ui, row);
            let mut draw_menus = |ui: &mut egui::Ui| {
                type MenuAction = fn(&mut Recho, &mut egui::Ui);
                let menus: [(&str, MenuAction); 8] = [
                    ("文件", |app, ui| {
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
                            .add_enabled(
                                !app.selection.is_empty(),
                                egui::Button::new("导出所选 HAR"),
                            )
                            .clicked()
                        {
                            app.choose_har(ui.ctx(), true);
                            ui.close();
                        }
                        if ui.button("退出").clicked() {
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    }),
                    ("编辑", |app, ui| {
                        if ui.button("复制所选 URL").clicked() {
                            copy_url(app, ui);
                            ui.close();
                        }
                    }),
                    ("视图", |app, ui| {
                        ui.checkbox(&mut app.sidebar_visible, "显示侧边栏");
                        if ui.checkbox(&mut app.details_open, "显示详情").changed()
                            && app.details_open
                        {
                            app.load_details();
                        }
                        ui.menu_button("主题", |ui| {
                            #[cfg(windows)]
                            ui.set_min_width(MENU_CONTENT_WIDTH);
                            for (color, label) in theme::ThemeColor::OPTIONS {
                                if ui
                                    .selectable_label(app.theme.color == color, label)
                                    .clicked()
                                {
                                    app.set_theme(theme::Theme { color, ..app.theme }, ui.ctx());
                                    ui.close();
                                }
                            }
                        });
                        ui.menu_button("外观", |ui| {
                            #[cfg(windows)]
                            ui.set_min_width(MENU_CONTENT_WIDTH);
                            for (appearance, label) in theme::Appearance::OPTIONS {
                                if ui
                                    .selectable_label(app.theme.appearance == appearance, label)
                                    .clicked()
                                {
                                    app.set_theme(
                                        theme::Theme {
                                            appearance,
                                            ..app.theme
                                        },
                                        ui.ctx(),
                                    );
                                    ui.close();
                                }
                            }
                        });
                    }),
                    ("工具", |app, ui| {
                        if ui.button("MCP").clicked() {
                            app.mcp.open();
                            ui.close();
                        }
                        if ui.button("访问令牌").clicked() {
                            app.mcp.open_tokens();
                            ui.close();
                        }
                    }),
                    ("调试", |app, ui| {
                        use crate::{network_ui::NetworkSection, rules_ui::RuleCategory};

                        if ui
                            .add_enabled(
                                app.ready,
                                egui::Button::new(if app.running {
                                    "停止调试"
                                } else {
                                    "启动调试"
                                })
                                .shortcut_text("Ctrl+G"),
                            )
                            .clicked()
                        {
                            app.toggle_capture();
                            ui.close();
                        }
                        if ui
                            .add_enabled(app.ready, egui::Button::new("清空捕获"))
                            .clicked()
                        {
                            app.clear_capture_space();
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
                            ui.close();
                        }
                        ui.separator();
                        for section in [
                            NetworkSection::Ssl,
                            NetworkSection::Mirror,
                            NetworkSection::Gateway,
                        ] {
                            if ui.button(section.label()).clicked() {
                                app.open_network(section);
                                ui.close();
                            }
                        }
                        ui.separator();
                        for category in [
                            RuleCategory::Rewrite,
                            RuleCategory::Breakpoint,
                            RuleCategory::Script,
                        ] {
                            if ui.button(category.label()).clicked() {
                                app.rules.open(category);
                                ui.close();
                            }
                        }
                        if ui
                            .add_enabled(
                                app.spaces.current().is_some(),
                                egui::Button::new("自动高亮"),
                            )
                            .clicked()
                        {
                            app.edit_highlights();
                            ui.close();
                        }
                        ui.separator();
                        for section in [NetworkSection::Reverse, NetworkSection::Access] {
                            if ui.button(section.label()).clicked() {
                                app.open_network(section);
                                ui.close();
                            }
                        }
                    }),
                    ("代理", |app, ui| {
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
                    }),
                    ("证书", |app, ui| {
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
                            #[cfg(windows)]
                            ui.set_min_width(MENU_CONTENT_WIDTH);
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
                    }),
                    ("帮助", |app, ui| {
                        if ui.button("关于 recho").clicked() {
                            app.about_open = true;
                            ui.close();
                        }
                    }),
                ];
                ui.horizontal_centered(|ui| {
                    #[cfg(windows)]
                    app.title_bar.icon(ui, 22.0);
                    #[cfg(not(windows))]
                    ui.label(RichText::new("recho").strong().color(colors.accent));
                    ui.add_space(2.0);
                    show_menu_titles(ui, menus.map(|(label, _)| label), |index, ui| {
                        (menus[index].1)(app, ui);
                    })
                })
                .inner
            };
            #[cfg(windows)]
            {
                let menu_end = draw_menus(&mut menu_ui);
                app.title_bar.show(ui, row, menu_end);
            }
            #[cfg(not(windows))]
            draw_menus(ui);
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
        .frame(
            egui::Frame::new()
                .fill(colors.bg)
                .inner_margin(egui::Margin::symmetric(14, 0)),
        )
        .show(ui, |ui| {
            proxy_bar(app, ui);
            egui::ScrollArea::horizontal()
                .id_salt("tabs")
                .show(ui, |ui| {
                    control_row(ui);
                    ui.horizontal(|ui| {
                        capture_tabs(app, ui);
                        app.composer.show_tabs(ui, &app.worker);
                        let add = ui.add_enabled(
                            app.ready,
                            egui::Button::new("＋").min_size(egui::Vec2::splat(CONTROL_HEIGHT)),
                        );
                        action_menu(
                            egui::Popup::menu(&add)
                                .id(ui.make_persistent_id("new_workbench_tab"))
                                .gap(4.0),
                            |ui| {
                                let shortcut = ui.ctx().format_shortcut(&NEW_HTTP);
                                if ui
                                    .add(egui::Button::new("新建 HTTP").shortcut_text(shortcut))
                                    .clicked()
                                {
                                    if app.navigation == Navigation::History {
                                        app.navigate(Navigation::Capture);
                                    }
                                    app.composer.open_new();
                                    ui.close();
                                }
                                if menu_item(ui, "新建 HTTP 报文", true) {
                                    if app.navigation == Navigation::History {
                                        app.navigate(Navigation::Capture);
                                    }
                                    app.composer.open_raw();
                                    ui.close();
                                }
                                if menu_item(ui, "新建 WebSocket", true) {
                                    app.composer.open_websocket();
                                    ui.close();
                                }
                                if menu_item(ui, "新建 gRPC", true) {
                                    app.composer.open_grpc();
                                    ui.close();
                                }
                                ui.menu_button("新建规范文件", |ui| {
                                    ui.set_width(120.0);
                                    ui.spacing_mut().interact_size.y = 26.0;
                                    if menu_item(ui, "Protobuf", true) {
                                        app.composer.open_protobuf();
                                        ui.close();
                                    }
                                });
                                ui.separator();
                                if menu_item(ui, "新建临时捕获", true) {
                                    app.navigate(Navigation::Capture);
                                    app.composer.show_capture();
                                    app.new_capture_space();
                                    ui.close();
                                }
                                ui.separator();
                                if menu_item(ui, "从剪贴板导入", true) {
                                    app.composer.import_clipboard(ui.ctx());
                                    ui.close();
                                }
                            },
                        );
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
            crate::capture_filter_ui::show(app, ui);
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
            history_sidebar(app, ui);
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
            ui.visuals_mut().collapsing_header_frame = true;
            if app.navigation == Navigation::Tools {
                section(ui, "调试", |ui| {
                    if sidebar_row(ui, "自动高亮", None).clicked() {
                        app.edit_highlights();
                    }
                    use crate::rules_ui::RuleCategory;
                    for (label, category) in [
                        ("全部规则", RuleCategory::All),
                        ("重写", RuleCategory::Rewrite),
                        ("断点", RuleCategory::Breakpoint),
                        ("脚本", RuleCategory::Script),
                    ] {
                        if sidebar_row(ui, label, None).clicked() {
                            app.rules.open(category);
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
                        if sidebar_row(ui, label, None).clicked() {
                            app.open_network(section);
                        }
                    }
                });
                section(ui, "本地工具", |ui| {
                    if sidebar_row(ui, "MCP", None).clicked() {
                        app.mcp.open();
                    }
                    if sidebar_row(ui, "访问令牌", None).clicked() {
                        app.mcp.open_tokens();
                    }
                    if sidebar_row(ui, "安装 CA 证书", None).clicked() {
                        app.certificates.open(CertificatePage::Local);
                    }
                });
            } else {
                section(ui, "规范文件", |ui| {
                    app.composer.show_specifications(ui);
                });
                app.library.show_sidebar(ui, &app.worker);
                section(ui, "设备", |ui| {
                    if sidebar_row(ui, "安装 CA 证书", None).clicked() {
                        app.certificates.open(CertificatePage::Local);
                    }
                });
                let apps: BTreeSet<_> =
                    app.rows.iter().map(|row| row.application.clone()).collect();
                section(ui, "应用程序", |ui| {
                    if apps.is_empty() {
                        ui.weak("暂无应用程序");
                    }
                    if !app.app_filter.is_empty() && sidebar_row(ui, "全部应用", None).clicked()
                    {
                        app.app_filter.clear();
                        app.refilter();
                    }
                    for name in apps {
                        if sidebar_row(ui, &name, Some(app.app_filter == name))
                            .on_hover_text(&name)
                            .clicked()
                        {
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
                    if !app.host_filter.is_empty() && sidebar_row(ui, "全部域名", None).clicked()
                    {
                        app.host_filter.clear();
                        app.refilter();
                    }
                    for host in hosts {
                        if sidebar_row(ui, &host, Some(app.host_filter == host))
                            .on_hover_text(&host)
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
    let colors = app.theme.palette(ui.ctx());
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(0, 8))
        .show(ui, |ui| {
            let size = egui::vec2(ui.available_width(), 34.0);
            let layout = egui::Layout::left_to_right(egui::Align::Center);
            ui.allocate_ui_with_layout(size, layout, |ui| {
                let address_ready = app.ready && app.port != 0;
                let text = if address_ready {
                    format!("代理地址  {}", app.proxy_address())
                } else {
                    "启动中…".into()
                };
                let stop_width = 75.0;
                let clear_width = 33.0;
                let gap = ui.spacing().item_spacing.x;
                let address = egui::Frame::new()
                    .fill(colors.chrome)
                    .stroke(egui::Stroke::new(1.0, colors.line))
                    .corner_radius(4)
                    .inner_margin(egui::Margin::symmetric(10, 2));
                let address_width = ui.available_width() - stop_width - clear_width - 2.0 * gap;
                let address_height = address
                    .show(ui, |ui| {
                        let width = (address_width - address.total_margin().sum().x).max(120.0);
                        ui.allocate_ui_with_layout(egui::vec2(width, 28.0), layout, |ui| {
                            let status = if address_ready && app.running {
                                colors.green
                            } else {
                                colors.muted
                            };
                            let (dot, painter) =
                                ui.allocate_painter(egui::vec2(6.0, 6.0), egui::Sense::hover());
                            painter.circle_filled(dot.rect.center(), 3.0, status);
                            let text_width = (ui.available_width() - 5.0 * (28.0 + gap)).max(0.0);
                            ui.allocate_ui_with_layout(
                                egui::vec2(text_width, 28.0),
                                layout,
                                |ui| {
                                    let label = ui.add(
                                        egui::Label::new(RichText::new(&text).color(status))
                                            .truncate(),
                                    );
                                    label.on_hover_text(&text);
                                },
                            );
                            let edit = ui.add_enabled(
                                address_ready,
                                egui::Button::new(icon("\u{e3c9}"))
                                    .min_size(egui::vec2(28.0, 28.0))
                                    .frame_when_inactive(false),
                            );
                            edit.widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::WidgetType::Button,
                                    address_ready,
                                    "修改端口号",
                                )
                            });
                            if edit.on_hover_text("修改端口号").clicked() {
                                app.port_dialog = Some(network_ui::PortDialog::new(app.port));
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if icon_button(ui, "\u{e9e0}", "安装 CA 证书", true).clicked()
                                    {
                                        app.certificates.open(CertificatePage::Local);
                                    }
                                    let (active, pending) = app.system_proxy;
                                    let title = if active {
                                        "系统代理已启用，点击恢复"
                                    } else if pending {
                                        "恢复系统代理"
                                    } else {
                                        "启用系统代理"
                                    };
                                    let enabled = app.ready && (app.running || pending);
                                    let button = egui::Button::selectable(
                                        active,
                                        if active {
                                            icon("\u{e894}").color(colors.bg)
                                        } else {
                                            icon("\u{e894}")
                                        },
                                    )
                                    .min_size(egui::vec2(28.0, 28.0));
                                    let proxy = ui.add_enabled(
                                        enabled,
                                        if active {
                                            button.fill(colors.accent)
                                        } else {
                                            button
                                        },
                                    );
                                    proxy.widget_info(|| {
                                        egui::WidgetInfo::selected(
                                            egui::WidgetType::Button,
                                            enabled,
                                            active,
                                            title,
                                        )
                                    });
                                    if proxy.on_hover_text(title).clicked() {
                                        app.send(Command::SystemProxy(!active && !pending));
                                    }
                                    if icon_button(ui, "\u{e14d}", "复制代理地址", address_ready)
                                        .clicked()
                                    {
                                        ui.ctx().copy_text(app.proxy_address());
                                    }
                                    let enabled = app.ready && app.spaces.settings.is_none();
                                    let active = app.spaces.incognito;
                                    let button = egui::Button::selectable(
                                        active,
                                        if active {
                                            icon("\u{e8f5}").color(colors.bg)
                                        } else {
                                            icon("\u{e8f5}")
                                        },
                                    )
                                    .min_size(egui::vec2(28.0, 28.0));
                                    let toggle = ui.add_enabled(
                                        enabled,
                                        if active {
                                            button.fill(colors.accent)
                                        } else {
                                            button
                                        },
                                    );
                                    toggle.widget_info(|| {
                                        egui::WidgetInfo::selected(
                                            egui::WidgetType::Button,
                                            enabled,
                                            active,
                                            "无痕抓包",
                                        )
                                    });
                                    if toggle.on_hover_text(if active {
                                        "无痕抓包已开启。新记录仅保存在内存中，清空后无法找回。"
                                    } else {
                                        "无痕抓包。开启后，新记录仅保存在内存中，清空后无法找回。"
                                    }).clicked() {
                                        app.set_incognito(!active);
                                    }
                                },
                            );
                        });
                    })
                    .response
                    .rect
                    .height();
                if ui
                    .add_enabled(
                        app.ready,
                        egui::Button::new(if app.running {
                            "■  停止"
                        } else {
                            "▶  调试"
                        })
                        .min_size(egui::vec2(stop_width, address_height)),
                    )
                    .clicked()
                {
                    app.toggle_capture();
                }
                let clear = ui.add_enabled(
                    app.ready,
                    egui::Button::new(icon("\u{e92e}"))
                        .min_size(egui::vec2(clear_width, address_height)),
                );
                clear.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, app.ready, "清空捕获")
                });
                if clear.on_hover_text("清空捕获（Ctrl+X）").clicked() {
                    app.clear_capture_space();
                }
            });
        });
}

fn filters(app: &mut Recho, ui: &mut egui::Ui) {
    let mut changed = false;
    let size = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
    let layout = egui::Layout::right_to_left(egui::Align::Center);
    ui.allocate_ui_with_layout(size, layout, |ui| {
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
        if !app.capture_filters.is_empty() {
            ui.weak(format!("MCP 筛选 ({})", app.capture_filters.len()));
        }
        egui::ScrollArea::horizontal()
            .id_salt("quick_filters")
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    if ui
                        .selectable_label(
                            app.protocol_filter.is_empty()
                                && app.version_filter.is_empty()
                                && app.type_filter.is_empty()
                                && app.status_filter == 0,
                            "All",
                        )
                        .clicked()
                    {
                        app.protocol_filter.clear();
                        app.version_filter.clear();
                        app.type_filter.clear();
                        app.status_filter = 0;
                        changed = true;
                    }
                    ui.separator();
                    for (filter, choices) in [
                        (
                            &mut app.protocol_filter,
                            &["HTTP", "HTTPS", "WebSocket", "SSE"][..],
                        ),
                        (&mut app.version_filter, &["HTTP1", "HTTP2", "HTTP3"][..]),
                    ] {
                        for &choice in choices {
                            if ui.selectable_label(*filter == choice, choice).clicked() {
                                *filter = if *filter == choice {
                                    String::new()
                                } else {
                                    choice.into()
                                };
                                changed = true;
                            }
                        }
                        ui.separator();
                    }
                    for (label, content_type) in [
                        ("JSON", "json"),
                        ("XML", "xml"),
                        ("文本", "text/plain"),
                        ("HTML", "html"),
                        ("JS", "javascript"),
                        ("图片", "image/"),
                        ("媒体", "video/"),
                        ("二进制", "binary"),
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
    let colors = app.theme.palette(ui.ctx());
    if app.spaces.items.is_empty() {
        ui.weak("调试");
        return;
    }
    let mut next = None;
    let mut remove = None;
    let mut groups: Vec<_> = app.spaces.items.keys().cloned().collect();
    if let Some(default) = groups.iter().position(|id| id == "default") {
        groups[..=default].rotate_right(1);
    }
    for id in groups {
        let space = app.spaces.items.get_mut(&id).unwrap();
        let current = id == app.spaces.group;
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
            .min_size(egui::vec2(100.0, CONTROL_HEIGHT))
            .wrap_mode(egui::TextWrapMode::Extend),
        );
        action_menu(
            egui::Popup::context_menu(&response)
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside),
            |ui| {
                ui.label("会话名称");
                ui.add(
                    egui::TextEdit::singleline(&mut space.name)
                        .desired_width(f32::INFINITY)
                        .char_limit(80),
                );
                ui.separator();
                if menu_item(ui, "关闭会话标签", id != "default") {
                    remove = Some(id.clone());
                    ui.close();
                }
            },
        );
        if response.clicked() {
            next = Some(id.clone());
        }
        if id != "default" {
            let close = ui.add(
                egui::Button::new("×")
                    .min_size(egui::vec2(26.0, CONTROL_HEIGHT))
                    .frame_when_inactive(false),
            );
            close.ctx.accesskit_node_builder(close.id, |node| {
                node.set_label(format!("关闭 {}", space.name))
            });
            if close.on_hover_text("关闭会话标签").clicked() {
                remove = Some(id.clone());
            }
        }
    }
    if let Some(id) = remove {
        app.close_capture_space(&id);
    }
    if let Some(id) = next {
        app.navigate(Navigation::Capture);
        app.composer.show_capture();
        app.activate_capture(id);
    }
}

fn select_all_pressed(input: &mut egui::InputState) -> bool {
    input.consume_key(egui::Modifiers::COMMAND, egui::Key::A)
        || input.consume_key(egui::Modifiers::CTRL, egui::Key::A)
}

fn history_sidebar(app: &mut Recho, ui: &mut egui::Ui) {
    let focus_id = ui.make_persistent_id("history_dates_focus");
    ui.interact(
        egui::Rect::NOTHING,
        focus_id,
        egui::Sense::focusable_noninteractive(),
    );
    let delete_enabled = !app.history_delete_pending && app.history_delete.is_none();
    if delete_enabled
        && ui.memory(|memory| memory.has_focus(focus_id))
        && ui.input_mut(select_all_pressed)
    {
        app.history_selected_dates = app
            .history_days
            .iter()
            .map(|day| day.date.clone())
            .collect();
        if app.history_date_anchor.is_none() {
            app.history_date_anchor = app.history_days.first().map(|day| day.date.clone());
        }
    }
    ui.horizontal(|ui| {
        ui.strong("历史");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add_enabled(
                    delete_enabled && !app.history_selected_dates.is_empty(),
                    egui::Button::new("删除所选"),
                )
                .clicked()
            {
                app.prepare_history_delete(HistoryDeletion::Dates(
                    app.history_selected_dates.iter().cloned().collect(),
                ));
            }
        });
    });
    let mut date = app.history_date.clone();
    let mut delete_day = None;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let all_selected = date.is_none() && app.history_selected_dates.is_empty();
            if sidebar_row(ui, "全部日期", Some(all_selected)).clicked() {
                ui.memory_mut(|memory| memory.request_focus(focus_id));
                date = None;
                app.history_selected_dates.clear();
                app.history_date_anchor = None;
            }
            for day in &app.history_days {
                ui.push_id(&day.date, |ui| {
                    ui.allocate_ui_with_layout(
                        egui::vec2(ui.available_width(), ui.spacing().interact_size.y),
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            let delete = ui.add_enabled(
                                delete_enabled,
                                egui::Button::new(icon("\u{e92e}").size(16.0))
                                    .frame_when_inactive(false),
                            );
                            delete.widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::WidgetType::Button,
                                    delete_enabled,
                                    format!("删除 {}", day.date),
                                )
                            });
                            if delete.on_hover_text("删除这一天").clicked() {
                                delete_day = Some(day.date.clone());
                            }
                            let selected = app.history_selected_dates.contains(&day.date);
                            if sidebar_row(
                                ui,
                                format!("{}  ({})", day.date, day.total),
                                Some(selected),
                            )
                            .clicked()
                            {
                                ui.memory_mut(|memory| memory.request_focus(focus_id));
                                let modifiers = ui.input(|input| input.modifiers);
                                let order = if modifiers.shift {
                                    app.history_days
                                        .iter()
                                        .map(|day| day.date.as_str())
                                        .collect()
                                } else {
                                    Vec::new()
                                };
                                select_list_item(
                                    &mut app.history_selected_dates,
                                    &mut app.history_date_anchor,
                                    &day.date,
                                    modifiers,
                                    &order,
                                );
                                if !modifiers.command && !modifiers.ctrl && !modifiers.shift {
                                    date = Some(day.date.clone());
                                }
                            }
                        },
                    );
                });
            }
        });
    if let Some(day) = delete_day {
        app.prepare_history_delete(HistoryDeletion::Dates(vec![day]));
    }
    if date != app.history_date {
        app.history_date = date;
        app.select(None);
        app.load_history(0);
    }
}

fn history(app: &mut Recho, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.strong("历史");
        let search_width = (ui.available_width() - 235.0).clamp(75.0, 260.0);
        if ui
            .add(
                egui::TextEdit::singleline(&mut app.history_query)
                    .hint_text("搜索历史")
                    .desired_width(search_width),
            )
            .changed()
        {
            app.select(None);
            app.load_history(0);
        }
        if ui
            .add_enabled(
                !app.selection.is_empty()
                    && !app.history_delete_pending
                    && app.history_delete.is_none(),
                egui::Button::new("删除所选"),
            )
            .clicked()
        {
            app.prepare_history_delete(HistoryDeletion::Selected(
                app.selection.iter().cloned().collect(),
            ));
        }
        if ui.button("刷新").clicked() {
            app.load_history(app.history_offset);
        }
        if ui
            .add_enabled(app.history_offset > 0, egui::Button::new("上一页"))
            .clicked()
        {
            app.select(None);
            app.load_history(app.history_offset.saturating_sub(100));
        }
        if ui
            .add_enabled(app.history_rows.len() == 100, egui::Button::new("下一页"))
            .clicked()
        {
            app.select(None);
            app.load_history(app.history_offset + 100);
        }
        ui.weak(format!("第 {} 页", app.history_offset / 100 + 1));
    });
    if app.history_load.is_some() {
        ui.spinner();
        return;
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
    let colors = app.theme.palette(ui.ctx());
    let history = app.navigation == Navigation::History;
    let focus_id = ui.make_persistent_id(("capture_focus", history));
    if !history
        && ui.memory(|memory| memory.focused().is_none() || memory.has_focus(focus_id))
        && ui.input_mut(clear_capture_pressed)
    {
        app.clear_capture_space();
    }
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
        if history && app.history_delete.is_none() && ui.input_mut(select_all_pressed) {
            app.selection = app.history_rows.iter().map(|row| row.id.clone()).collect();
            if app.selection_anchor.is_none() {
                app.selection_anchor = app.history_rows.first().map(|row| row.id.clone());
            }
        }
        let movement = ui.input_mut(|input| {
            input.count_and_consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) as isize
                - input.count_and_consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) as isize
        });
        if movement != 0 {
            let ids = visible_rows(&app.rows, &app.visible, &app.history_rows, history);
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
    let rows = visible_rows(&app.rows, &app.visible, &app.history_rows, history);
    let application_icons = &mut app.application_icons;
    egui::ScrollArea::horizontal()
        .id_salt(("table_horizontal", history))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_min_width(980.0);
            ui.visuals_mut().widgets.hovered.bg_fill = colors.chrome;
            ui.visuals_mut().widgets.noninteractive.bg_stroke.color =
                colors.bg.lerp_to_gamma(colors.line, 0.55);
            let cyan = if ui.visuals().dark_mode {
                egui::Color32::from_rgb(107, 216, 225)
            } else {
                egui::Color32::from_rgb(0, 104, 117)
            };
            let table = TableBuilder::new(ui)
                .id_salt(("captures", history))
                .resizable(true)
                .striped(true)
                .sense(egui::Sense::click())
                .auto_shrink([false, false])
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::initial(48.0).clip(true))
                .column(Column::initial(36.0).clip(true))
                .column(Column::initial(62.0).clip(true))
                .column(
                    Column::remainder()
                        .at_least(230.0)
                        .clip(true)
                        .resizable(false),
                )
                .column(Column::initial(108.0).clip(true))
                .column(Column::initial(65.0).clip(true))
                .column(Column::initial(112.0).clip(true))
                .column(Column::initial(75.0).clip(true))
                .column(Column::initial(75.0).clip(true).resizable(false));
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
                            capture_cell(ui, index, |ui| {
                                let title = RichText::new(*title).size(12.0);
                                if history {
                                    ui.add(egui::Label::new(title).truncate());
                                    return;
                                }
                                let active = app.sort_column == index;
                                let mut label = egui::Atoms::new(title);
                                if active {
                                    let chevron = if app.ascending {
                                        "\u{e5ce}"
                                    } else {
                                        "\u{e5cf}"
                                    };
                                    label.push_right(icon(chevron).size(12.0).color(colors.muted));
                                }
                                ui.spacing_mut().item_spacing.x = 1.0;
                                ui.spacing_mut().button_padding.x = 3.0;
                                if ui
                                    .add(
                                        egui::Button::new(label)
                                            .frame_when_inactive(false)
                                            .truncate(),
                                    )
                                    .clicked()
                                {
                                    sort = Some(index);
                                }
                            })
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
                        let highlight = (!history && app.spaces.marked.contains(&row.id))
                            .then(|| app.spaces.current().map(|space| space.highlight_color()))
                            .flatten();
                        let marked_color = match highlight {
                            Some("红色") => Some(colors.red),
                            Some("黄色") => Some(colors.warning),
                            Some("绿色") => Some(colors.green),
                            Some("蓝色") => Some(colors.key),
                            Some("青色") => Some(cyan),
                            _ => None,
                        };
                        for (index, text) in cells.into_iter().enumerate() {
                            table_row.col(|ui| {
                                capture_cell(ui, index, |ui| {
                                    let mut text =
                                        RichText::new(text).size(12.0).color(if index == 5 {
                                            color
                                        } else {
                                            marked_color
                                                .unwrap_or_else(|| ui.visuals().text_color())
                                        });
                                    if highlight == Some("划线") {
                                        text = text.strikethrough();
                                    }
                                    if index == 4 {
                                        ui.spacing_mut().item_spacing.x = 5.0;
                                        let size = egui::Vec2::splat(14.0);
                                        if let Some(texture) = application_icons
                                            .get(ui.ctx(), row.application_path.as_deref())
                                        {
                                            ui.image((texture.id(), size));
                                        } else {
                                            ui.add_sized(
                                                size,
                                                egui::Label::new(
                                                    icon("\u{e069}")
                                                        .size(size.y)
                                                        .color(colors.muted),
                                                )
                                                .selectable(false),
                                            );
                                        }
                                    }
                                    ui.add(egui::Label::new(text).truncate().selectable(false));
                                })
                            });
                        }
                        let response = table_row.response();
                        if response.clicked() {
                            choose = Some((row.id.clone(), modifiers));
                        }
                        if response.secondary_clicked() && !app.selection.contains(&row.id) {
                            choose = Some((row.id.clone(), egui::Modifiers::NONE));
                        }
                        if response.double_clicked()
                            && !modifiers.command
                            && !modifiers.ctrl
                            && !modifiers.shift
                        {
                            choose = Some((row.id.clone(), egui::Modifiers::NONE));
                            open = true;
                        }
                        action_menu(egui::Popup::context_menu(&response), |ui| {
                            if menu_item(ui, "查看详情", true) {
                                choose = Some((row.id.clone(), egui::Modifiers::NONE));
                                open = true;
                                ui.close();
                            }
                            if menu_item(ui, "编辑并重发", true) {
                                edit = Some(row.id.clone());
                                ui.close();
                            }
                            ui.separator();
                            if menu_item(ui, "复制 URL", true) {
                                ui.ctx().copy_text(row.url.clone());
                                ui.close();
                            }
                            if menu_item(ui, "复制 cURL", true) {
                                curl = Some(row.id.clone());
                                ui.close();
                            }
                            if menu_item(ui, "导出所选 HAR", true) {
                                export = true;
                                ui.close();
                            }
                            ui.separator();
                            for (caption, category) in [
                                ("添加重写规则", crate::rules_ui::RuleCategory::Rewrite),
                                ("添加断点", crate::rules_ui::RuleCategory::Breakpoint),
                                ("添加脚本", crate::rules_ui::RuleCategory::Script),
                            ] {
                                if menu_item(ui, caption, true) {
                                    rule = Some((category, row.method.clone(), row.url.clone()));
                                    ui.close();
                                }
                            }
                            ui.separator();
                            for (caption, compare, now) in [
                                ("添加书签", false, false),
                                ("添加到对比池", true, false),
                                ("对比所选", true, true),
                            ] {
                                if menu_item(
                                    ui,
                                    caption,
                                    !row.incognito && (!now || app.selection.len() >= 2),
                                ) {
                                    keep = Some((compare, now));
                                    ui.close();
                                }
                            }
                            if !history {
                                ui.separator();
                                if menu_item(ui, "从此会话移除所选", true) {
                                    hide = true;
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
        app.rules.new_for_capture(category, &method, &url);
    }
}

fn capture_cell(ui: &mut egui::Ui, column: usize, contents: impl FnOnce(&mut egui::Ui)) {
    let layout = match column {
        0 | 1 | 5 => egui::Layout::top_down(egui::Align::Center)
            .with_main_align(egui::Align::Center)
            .with_main_justify(true),
        7 | 8 => egui::Layout::right_to_left(egui::Align::Center),
        _ => egui::Layout::left_to_right(egui::Align::Center),
    };
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(7, 0))
        .show(ui, |ui| {
            ui.with_layout(layout, contents);
        });
}

pub(super) fn visible_rows<'a>(
    rows: &'a [Row],
    visible: &[usize],
    archived: &'a [Row],
    history: bool,
) -> Vec<&'a Row> {
    if history {
        archived.iter().collect()
    } else {
        visible.iter().map(|&index| &rows[index]).collect()
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
    use super::{clear_capture_pressed, egui, moved_index, select_list_item};

    #[test]
    fn capture_navigation_does_not_wrap_and_can_start_without_selection() {
        assert_eq!(moved_index(0, None, -1), None);
        assert_eq!(moved_index(4, None, -1), Some(3));
        assert_eq!(moved_index(4, None, 1), Some(0));
        assert_eq!(moved_index(4, Some(0), -1), Some(0));
        assert_eq!(moved_index(4, Some(3), 1), Some(3));
        assert_eq!(moved_index(80, Some(15), 45), Some(60));
    }

    #[test]
    fn clear_shortcut_accepts_windows_cut_event_with_ctrl() {
        for (modifiers, expected) in [
            (
                egui::Modifiers {
                    command: true,
                    ..egui::Modifiers::CTRL
                },
                true,
            ),
            (egui::Modifiers::SHIFT, false),
        ] {
            let ctx = egui::Context::default();
            ctx.run_ui(
                egui::RawInput {
                    events: vec![egui::Event::ModifiersChanged(modifiers), egui::Event::Cut],
                    ..Default::default()
                },
                |ui| {
                    assert_eq!(ui.input_mut(clear_capture_pressed), expected);
                },
            )
            .textures_delta
            .clear();
        }
    }

    #[test]
    fn list_selection_toggles_rows_and_extends_in_display_order() {
        let order = ["newest", "second", "third", "oldest"];
        let mut selected = std::collections::BTreeSet::new();
        let mut anchor = None;
        select_list_item(
            &mut selected,
            &mut anchor,
            "newest",
            egui::Modifiers::NONE,
            &order,
        );
        select_list_item(
            &mut selected,
            &mut anchor,
            "third",
            egui::Modifiers::CTRL,
            &order,
        );
        assert_eq!(selected, ["newest".into(), "third".into()].into());
        select_list_item(
            &mut selected,
            &mut anchor,
            "oldest",
            egui::Modifiers::SHIFT,
            &order,
        );
        assert_eq!(selected, ["oldest".into(), "third".into()].into());
        select_list_item(
            &mut selected,
            &mut anchor,
            "third",
            egui::Modifiers::CTRL,
            &order,
        );
        assert_eq!(selected, ["oldest".into()].into());
    }
}

fn details(app: &mut Recho, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.strong("详情");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add(egui::Button::new("×").frame_when_inactive(false))
                .on_hover_text("关闭详情")
                .clicked()
            {
                app.details_open = false;
            }
            if ui
                .add(egui::Button::new("重新读取").frame_when_inactive(false))
                .clicked()
            {
                app.select(app.selected.clone());
            }
        });
    });
    ui.separator();
    let height = ui.available_height();
    let minimum = 120.0_f32.min(height / 2.0);
    egui::Panel::top("request")
        .default_size(height * 0.48)
        .min_size(minimum)
        .max_size(height - minimum)
        .resizable(true)
        .show(ui, |ui| {
            ui.take_available_space();
            message(app, ui, 0);
        });
    message(app, ui, 1);
}
fn message(app: &mut Recho, ui: &mut egui::Ui, index: usize) {
    let colors = app.theme.palette(ui.ctx());
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
        if !request {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let more =
                    body::response::action(ui, "\u{e5d3}", "更多响应操作", panel.viewer.is_some());
                if let Some(viewer) = &panel.viewer {
                    action_menu(egui::Popup::menu(&more), |ui| {
                        if menu_item(ui, "复制响应头", !viewer.detail.headers.is_empty()) {
                            ui.ctx().copy_text(viewer.detail.headers.clone());
                            ui.close();
                        }
                        let text = match &viewer.content {
                            Content::Text { text, .. } if viewer.detail.notice.is_none() => {
                                Some(text)
                            }
                            _ => None,
                        };
                        if menu_item(ui, "复制响应头和正文", text.is_some()) {
                            if let Some(text) = text {
                                ui.ctx().copy_text(format!(
                                    "{}\r\n\r\n{text}",
                                    viewer.detail.headers.trim_end_matches(['\r', '\n']),
                                ));
                            }
                            ui.close();
                        }
                    });
                }
                if row.status > 0 {
                    let color = if row.error.is_some() || row.status >= 400 {
                        colors.red
                    } else {
                        colors.green
                    };
                    response_badge(ui, row.status.to_string(), color, colors.bg, "HTTP 状态码");
                }
                if !row.protocol.is_empty() {
                    let protocol = match row.protocol.as_str() {
                        "HTTP/2" | "HTTP/2.0" => "h2",
                        "HTTP/3" | "HTTP/3.0" => "h3",
                        protocol => protocol,
                    };
                    response_badge(ui, protocol, colors.green, colors.bg, &row.protocol);
                }
            });
        }
    });
    ui.separator();
    if request {
        ui.add(egui::Label::new(&row.url).truncate());
    } else {
        ui.horizontal(|ui| {
            ui.weak(format!("{} ms", row.duration_ms));
            ui.separator();
            ui.weak(format!("{} bytes", row.response_bytes));
        });
    }
    if let Some(error) = &row.error {
        ui.colored_label(colors.red, error);
    }
    ui.separator();
    if panel.tab == MessageTab::Overview {
        egui::ScrollArea::vertical()
            .id_salt("request_overview")
            .min_scrolled_height(0.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (key, value) in [
                    ("方法", row.method.as_str()),
                    ("协议", row.protocol.as_str()),
                    ("应用程序", row.application.as_str()),
                    ("服务器 IP", row.server_ip.as_str()),
                    ("状态", &row.status.to_string()),
                    ("URL", row.url.as_str()),
                ] {
                    ui.horizontal(|ui| {
                        ui.add_sized([85.0, 20.0], egui::Label::new(RichText::new(key).weak()));
                        ui.add(egui::Label::new(value).wrap());
                    });
                }
            });
        return;
    }
    let Some(viewer) = &mut panel.viewer else {
        ui.label("正在读取归档…");
        return;
    };
    if panel.tab == MessageTab::Headers {
        egui::ScrollArea::both()
            .id_salt(("headers", index))
            .min_scrolled_height(0.0)
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
    if !request {
        ui.push_id((&viewer.detail.id, "response"), |ui| {
            viewer.response.show(
                ui,
                &mut panel.format,
                &viewer.content,
                viewer.detail.notice.as_deref(),
                (panel.tab == MessageTab::Raw).then_some(viewer.detail.headers.as_str()),
                &row.content_type,
                &mut app.response_save,
            );
        });
        return;
    }
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(2, 3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                let is_text = matches!(viewer.content, Content::Text { .. });
                for (format, label) in [
                    (BodyFormat::Raw, "Raw"),
                    (BodyFormat::Json, "JSON"),
                    (BodyFormat::Tree, "Tree"),
                    (BodyFormat::Hex, "Hex"),
                ] {
                    if ui
                        .add_enabled(
                            is_text || format == BodyFormat::Hex,
                            egui::Button::selectable(panel.format == format, label)
                                .min_size(egui::vec2(38.0, 27.0)),
                        )
                        .clicked()
                    {
                        panel.format = format;
                    }
                }
                if let Content::Text { text, .. } = &viewer.content {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let label = if viewer.detail.notice.is_none() {
                            "复制完整正文"
                        } else {
                            "复制预览"
                        };
                        if ui
                            .add(
                                egui::Button::new(label)
                                    .min_size(egui::vec2(0.0, 27.0))
                                    .frame_when_inactive(false),
                            )
                            .clicked()
                        {
                            ui.ctx().copy_text(text.clone());
                        }
                    });
                }
            });
        });
    if let Some(notice) = &viewer.detail.notice {
        ui.colored_label(colors.warning, notice);
    }
    if panel.tab == MessageTab::Raw {
        let has_body = !viewer.content.bytes().is_empty();
        body::show_raw_headers(ui, &viewer.detail.headers, has_body);
        if !has_body {
            return;
        }
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
    egui::ScrollArea::both()
        .id_salt(("body", index, &viewer.detail.id, hex))
        .min_scrolled_height(0.0)
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

fn response_badge(
    ui: &mut egui::Ui,
    text: impl Into<String>,
    foreground: egui::Color32,
    background: egui::Color32,
    title: &str,
) {
    egui::Frame::new()
        .fill(background.lerp_to_gamma(foreground, 0.13))
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(11.0).strong().color(foreground))
        })
        .response
        .on_hover_text(title);
}

fn dialogs(app: &mut Recho, ctx: &egui::Context) {
    #[cfg(not(target_os = "macos"))]
    if app.about_open {
        let modal = Theme::modal(ctx, egui::Id::new("about_recho")).show(ctx, |ui| {
            ui.set_width(280.0);
            let close = dialog_header(ui, "关于 recho", true);
            ui.vertical_centered(|ui| {
                ui.add_space(12.0);
                #[cfg(windows)]
                app.title_bar.icon(ui, 64.0);
                ui.heading("recho");
                ui.label(concat!("版本 ", env!("CARGO_PKG_VERSION")));
                ui.add_space(16.0);
            });
            close
        });
        if modal.inner || modal.should_close() {
            app.about_open = false;
        }
    }
    app.network.show(ctx, &app.worker);
    crate::capture_filter_ui::highlight_dialog(app, ctx);
    if app.connection_open {
        let modal = Theme::modal(ctx, egui::Id::new("connection_settings")).show(ctx, |ui| {
            ui.set_width(480.0);
            let close = dialog_header(ui, "连接设置", true);
            let address_ready = app.ready && app.port != 0;
            ui.horizontal(|ui| {
                ui.label(if address_ready {
                    format!("代理地址  {}", app.proxy_address())
                } else {
                    "正在启动引擎…".into()
                });
                if ui
                    .add_enabled(address_ready, egui::Button::new("修改端口号"))
                    .clicked()
                {
                    app.port_dialog = Some(network_ui::PortDialog::new(app.port));
                }
                if ui
                    .add_enabled(address_ready, egui::Button::new("复制代理地址"))
                    .clicked()
                {
                    ctx.copy_text(app.proxy_address());
                }
            });
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
            close
        });
        if app.port_dialog.is_none()
            && !app.confirm_proxy_auto
            && (modal.inner || modal.should_close())
        {
            app.connection_open = false;
        }
    }
    if let Some(dialog) = &mut app.port_dialog
        && dialog.show(ctx, &app.worker, app.theme.palette(ctx), app.port)
    {
        app.port_dialog = None;
    }
    if let Some(intercept_tls) = app.certificates.show(
        ctx,
        app.theme.palette(ctx),
        app.port,
        app.running,
        app.ready,
    ) {
        app.send(Command::Configure {
            running: None,
            intercept_tls: Some(intercept_tls),
        });
    }
    if app.confirm_proxy_auto {
        let modal = Theme::modal(ctx, egui::Id::new("proxy_auto_confirmation")).show(ctx, |ui| {
            ui.set_width(400.0);
            let close = dialog_header(ui, "随调试启动启用系统代理", true);
            ui.label("下次启动 recho 或恢复调试时，recho 会将选中的协议指向本机监听端口。暂停、退出或手动恢复时会恢复原设置。");
            let colors = app.theme.palette(ctx);
            dialog_footer(ui, |ui| {
                let enable = ui.add(colors.dialog_action("启用", colors.accent)).clicked();
                let cancel = ui.button("取消").clicked();
                (enable, close || cancel)
            })
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
    if let Some(plan) = &app.history_delete {
        let modal = Theme::modal(ctx, egui::Id::new("delete_history")).show(ctx, |ui| {
            ui.heading("永久删除历史记录");
            let count = plan.count();
            ui.label(match plan.dates.as_slice() {
                [] => format!("永久删除所选的 {count} 条记录及正文？"),
                [date] => format!("永久删除 {date} 的全部 {count} 条记录及正文？"),
                dates => format!(
                    "永久删除所选 {} 天的全部 {count} 条记录及正文？",
                    dates.len()
                ),
            });
            if plan.active > 0 {
                ui.label(format!("其中 {} 条会同时从实时列表移除。", plan.active));
            }
            ui.horizontal(|ui| (ui.button("永久删除").clicked(), ui.button("取消").clicked()))
                .inner
        });
        if modal.inner.0 {
            let plan = app.history_delete.take().unwrap();
            app.confirm_history_delete(plan);
        } else if modal.inner.1 || modal.should_close() {
            app.history_delete = None;
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
