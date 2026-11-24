use super::{Navigation, Recho, platform::ProxyProtocol, theme::Theme, worker::Command};
use crate::{certificate_ui::Page as CertificatePage, platform::CertificateFormat};
use crate::{network_ui::NetworkSection, rules_ui::RuleCategory};
use eframe::egui;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, rc::Retained, sel};
use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
use objc2_foundation::{MainThreadMarker, NSObject, NSString};
use std::cell::RefCell;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Http,
    Raw,
    WebSocket,
    Grpc,
    Protobuf,
    CaptureSpace,
    ImportHar,
    ExportHar,
    Save,
    CloseTab,
    CopyUrl,
    Capture,
    Collections,
    Environments,
    History,
    Tools,
    Library,
    Mcp,
    McpTokens,
    Sidebar,
    Details,
    Theme(Theme),
    ToggleCapture,
    Clear,
    Incognito,
    Rules(RuleCategory),
    Network(NetworkSection),
    Highlights,
    SystemProxy,
    ProxyProtocol(ProxyProtocol),
    ProxyAuto,
    Connection,
    Intercept,
    Certificates(CertificatePage),
    ExportCertificate(CertificateFormat),
    About,
    Quit,
}

struct MenuTargetState {
    pending: RefCell<Vec<Action>>,
    items: RefCell<Vec<(Action, Retained<NSMenuItem>)>>,
    ctx: egui::Context,
}

// AppKit calls this target only on the main thread. Menu actions enter the same workbench state as clicks.
define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = MenuTargetState]
    struct RechoMenuTarget;

    impl RechoMenuTarget {
        #[unsafe(method(rechoSelect:))]
        fn select(&self, item: &NSMenuItem) {
            if let Some((action, _)) = self.ivars().items.borrow().get(item.tag() as usize) {
                let mut pending = self.ivars().pending.borrow_mut();
                if pending.len() < 32 {
                    pending.push(*action);
                }
                self.ivars().ctx.request_repaint();
            }
        }
    }
);

thread_local! {
    // NSMenuItem target is weak; retain the target for the lifetime of the AppKit application.
    static TARGET: RefCell<Option<Retained<RechoMenuTarget>>> = const { RefCell::new(None) };
}

fn top(mtm: MainThreadMarker, menu: &NSMenu, title: &str) -> Retained<NSMenu> {
    let title = NSString::from_str(title);
    let item = NSMenuItem::new(mtm);
    item.setTitle(&title);
    let submenu = NSMenu::new(mtm);
    submenu.setTitle(&title);
    submenu.setAutoenablesItems(false);
    item.setSubmenu(Some(&submenu));
    menu.addItem(&item);
    submenu
}

fn install(mtm: MainThreadMarker, ctx: &egui::Context) -> Retained<RechoMenuTarget> {
    let native = NSApplication::sharedApplication(mtm);
    let menu = native.mainMenu().unwrap_or_else(|| {
        let menu = NSMenu::new(mtm);
        top(mtm, &menu, "recho");
        native.setMainMenu(Some(&menu));
        menu
    });
    let target = RechoMenuTarget::alloc(mtm).set_ivars(MenuTargetState {
        pending: RefCell::new(Vec::new()),
        items: RefCell::new(Vec::new()),
        ctx: ctx.clone(),
    });
    // NSObject's init takes no parameters and initializes the allocated subclass.
    let target: Retained<RechoMenuTarget> = unsafe { msg_send![super(target), init] };
    let add = |menu: &NSMenu, title: &str, key: &str, action| {
        let mut items = target.ivars().items.borrow_mut();
        // The target owns this selector and uses the provided menu item argument.
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc(),
                &NSString::from_str(title),
                Some(sel!(rechoSelect:)),
                &NSString::from_str(key),
            )
        };
        if key
            .chars()
            .next()
            .is_some_and(|c| ('\u{f704}'..='\u{f70f}').contains(&c))
        {
            item.setKeyEquivalentModifierMask(NSEventModifierFlags::empty());
        }
        item.setTag(items.len() as isize);
        unsafe { item.setTarget(Some(&target)) };
        menu.addItem(&item);
        items.push((action, item));
    };
    let sep = |menu: &NSMenu| menu.addItem(&NSMenuItem::separatorItem(mtm));
    if let Some(app_menu) = menu.itemAtIndex(0).and_then(|item| item.submenu()) {
        // Route Cmd+Q through the workbench's normal close and save flow.
        if let Some(quit) = (0..app_menu.numberOfItems())
            .filter_map(|index| app_menu.itemAtIndex(index))
            .find(|item| item.action() == Some(sel!(terminate:)))
        {
            app_menu.removeItem(&quit);
        }
        add(&app_menu, "退出 recho", "q", Action::Quit);
    }
    let file = top(mtm, &menu, "文件");
    for (title, key, action) in [
        ("新建 HTTP", "t", Action::Http),
        ("新建 HTTP 报文", "", Action::Raw),
        ("新建 WebSocket", "", Action::WebSocket),
        ("新建 gRPC", "", Action::Grpc),
        ("新建 Protobuf 规范", "", Action::Protobuf),
    ] {
        add(&file, title, key, action);
    }
    sep(&file);
    add(&file, "新建临时捕获", "", Action::CaptureSpace);
    sep(&file);
    add(&file, "导入 HAR…", "o", Action::ImportHar);
    add(&file, "导出所选 HAR…", "", Action::ExportHar);
    sep(&file);
    add(&file, "保存", "s", Action::Save);
    add(&file, "关闭标签", "w", Action::CloseTab);
    let edit = top(mtm, &menu, "编辑");
    add(&edit, "复制所选 URL", "", Action::CopyUrl);
    let tools = top(mtm, &menu, "工具");
    add(&tools, "调试工具", "", Action::Tools);
    add(&tools, "书签与对比池", "", Action::Library);
    add(&tools, "MCP", "", Action::Mcp);
    add(&tools, "访问令牌", "", Action::McpTokens);
    let view = top(mtm, &menu, "视图");
    for (title, key, action) in [
        ("工作台", "\u{f704}", Action::Capture),
        ("集合", "\u{f705}", Action::Collections),
        ("环境变量", "\u{f706}", Action::Environments),
        ("历史记录", "\u{f707}", Action::History),
    ] {
        add(&view, title, key, action);
    }
    sep(&view);
    add(&view, "显示侧边栏", "", Action::Sidebar);
    add(&view, "显示详情", "", Action::Details);
    let themes = top(mtm, &view, "主题");
    for theme in Theme::ALL {
        add(&themes, theme.name(), "", Action::Theme(theme));
    }
    let debug = top(mtm, &menu, "调试");
    add(&debug, "启动调试", "g", Action::ToggleCapture);
    add(&debug, "清空捕获", "", Action::Clear);
    add(&debug, "无痕模式", "", Action::Incognito);
    sep(&debug);
    add(&debug, "SSL 配置", "", Action::Network(NetworkSection::Ssl));
    add(&debug, "镜像", "", Action::Network(NetworkSection::Mirror));
    add(&debug, "网关", "", Action::Network(NetworkSection::Gateway));
    sep(&debug);
    add(&debug, "重写", "", Action::Rules(RuleCategory::Rewrite));
    add(&debug, "断点", "", Action::Rules(RuleCategory::Breakpoint));
    add(&debug, "脚本", "", Action::Rules(RuleCategory::Script));
    add(&debug, "自动高亮", "", Action::Highlights);
    sep(&debug);
    add(
        &debug,
        "反向代理",
        "",
        Action::Network(NetworkSection::Reverse),
    );
    add(
        &debug,
        "访问控制",
        "",
        Action::Network(NetworkSection::Access),
    );
    let proxy = top(mtm, &menu, "代理");
    add(&proxy, "系统代理", "\u{f70f}", Action::SystemProxy);
    add(&proxy, "随调试启动启用系统代理", "", Action::ProxyAuto);
    sep(&proxy);
    for protocol in ProxyProtocol::ALL {
        add(
            &proxy,
            protocol.label(),
            "",
            Action::ProxyProtocol(protocol),
        );
    }
    sep(&proxy);
    add(&proxy, "连接设置", "", Action::Connection);
    add(
        &proxy,
        "二级代理",
        "",
        Action::Network(NetworkSection::Upstream),
    );
    let certificates = top(mtm, &menu, "证书");
    add(&certificates, "HTTPS 解密", "", Action::Intercept);
    certificates.addItem(&NSMenuItem::separatorItem(mtm));
    for (page, label) in CertificatePage::MENU {
        add(&certificates, label, "", Action::Certificates(page));
    }
    certificates.addItem(&NSMenuItem::separatorItem(mtm));
    let exports = top(mtm, &certificates, "导出 CA 证书");
    for (format, label) in CertificatePage::EXPORTS {
        add(&exports, label, "", Action::ExportCertificate(format));
    }
    let help = top(mtm, &menu, "帮助");
    add(&help, "关于 recho", "", Action::About);
    target
}

pub(super) fn show(app: &mut Recho, ui: &mut egui::Ui) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let target = TARGET
        .with_borrow_mut(|target| target.get_or_insert_with(|| install(mtm, ui.ctx())).clone());
    let actions = std::mem::take(&mut *target.ivars().pending.borrow_mut());
    for action in actions {
        run(app, ui.ctx(), mtm, action);
    }
    for (action, item) in target.ivars().items.borrow().iter() {
        let enabled = match action {
            Action::Http
            | Action::Raw
            | Action::WebSocket
            | Action::Grpc
            | Action::Protobuf
            | Action::CaptureSpace
            | Action::ImportHar
            | Action::ToggleCapture
            | Action::Clear
            | Action::Intercept
            | Action::Certificates(_)
            | Action::ExportCertificate(_) => app.ready,
            Action::Save => app.composer.active(),
            Action::CloseTab => app.mcp.is_open() || app.composer.active(),
            Action::ExportHar => app.ready && !app.selection.is_empty() && app.har_file.is_none(),
            Action::CopyUrl => app.selected.is_some(),
            Action::SystemProxy => app.ready && (app.running || app.system_proxy.1),
            Action::ProxyProtocol(protocol) => {
                protocol.supported() && app.ready && !app.proxy_preferences_saving
            }
            Action::ProxyAuto => app.ready && !app.proxy_preferences_saving,
            Action::Incognito => app.ready && app.spaces.settings.is_none(),
            Action::Highlights => app.spaces.current().is_some(),
            _ => true,
        };
        if item.isEnabled() != enabled {
            item.setEnabled(enabled);
        }
        let checked = match action {
            Action::SystemProxy => app.system_proxy.0,
            Action::ProxyProtocol(protocol) => protocol.enabled(app.proxy_preferences),
            Action::ProxyAuto => app.proxy_preferences.auto,
            Action::Intercept => app.intercept_tls,
            Action::Sidebar => app.sidebar_visible,
            Action::Details => app.details_open,
            Action::Theme(theme) => app.theme == *theme,
            Action::Incognito => app.spaces.incognito,
            _ => false,
        };
        if item.state() != isize::from(checked) {
            item.setState(isize::from(checked));
        }
        let title = match action {
            Action::SystemProxy if app.system_proxy.1 && !app.system_proxy.0 => "恢复系统代理",
            Action::SystemProxy => "系统代理",
            Action::ToggleCapture if app.running => "停止调试",
            Action::ToggleCapture => "启动调试",
            _ => continue,
        };
        if item.title().to_string() != title {
            item.setTitle(&NSString::from_str(title));
        }
    }
}

fn run(app: &mut Recho, ctx: &egui::Context, mtm: MainThreadMarker, action: Action) {
    if matches!(
        action,
        Action::Http | Action::Raw | Action::WebSocket | Action::Grpc | Action::Protobuf
    ) {
        app.navigate(Navigation::Capture);
    }
    match action {
        Action::Http => app.composer.open_new(),
        Action::Raw => app.composer.open_raw(),
        Action::WebSocket => app.composer.open_websocket(),
        Action::Grpc => app.composer.open_grpc(),
        Action::Protobuf => app.composer.open_protobuf(),
        Action::CaptureSpace => {
            app.navigate(Navigation::Capture);
            app.composer.show_capture();
            app.new_capture_space();
        }
        Action::ImportHar => app.choose_har(ctx, false),
        Action::ExportHar => app.choose_har(ctx, true),
        Action::Save => app.composer.save_active(&app.worker),
        Action::CloseTab => {
            if app.mcp.is_open() {
                app.mcp.back();
            } else {
                app.composer.close_active(&app.worker);
            }
        }
        Action::CopyUrl => {
            if let Some(row) = app
                .active_rows()
                .iter()
                .find(|row| app.selected.as_ref() == Some(&row.id))
            {
                ctx.copy_text(row.url.clone());
            }
        }
        Action::Capture => {
            app.navigate(Navigation::Capture);
            app.composer.show_capture();
        }
        Action::Collections => app.navigate(Navigation::Collections),
        Action::Environments => app.navigate(Navigation::Environments),
        Action::History => app.navigate(Navigation::History),
        Action::Tools => app.navigate(Navigation::Tools),
        Action::Library => app.library.open(false),
        Action::Mcp => app.mcp.open(),
        Action::McpTokens => app.mcp.open_tokens(),
        Action::Sidebar => app.sidebar_visible = !app.sidebar_visible,
        Action::Theme(theme) => app.set_theme(theme, ctx),
        Action::Details => {
            app.details_open = !app.details_open;
            if app.details_open {
                app.load_details();
            }
        }
        Action::ToggleCapture => app.toggle_capture(),
        Action::Clear => app.clear_confirmation = true,
        Action::Incognito => app.set_incognito(!app.spaces.incognito),
        Action::Rules(category) => {
            app.navigate(Navigation::Tools);
            app.rules.open(category);
            app.rules_open = true;
            app.network_open = false;
        }
        Action::Network(section) => app.open_network(section),
        Action::Highlights => {
            app.condition_editor = app
                .spaces
                .current()
                .map(|space| (true, space.highlights.clone()))
        }
        Action::SystemProxy => app.send(Command::SystemProxy(
            !app.system_proxy.0 && !app.system_proxy.1,
        )),
        Action::ProxyProtocol(protocol) => {
            app.set_proxy_preferences(protocol.toggled(app.proxy_preferences))
        }
        Action::ProxyAuto => app.toggle_proxy_auto(),
        Action::Connection => app.connection_open = true,
        Action::Certificates(page) => app.certificates.open(page),
        Action::ExportCertificate(format) => app.certificates.export(ctx, format),
        Action::Intercept => app.send(Command::Configure {
            running: app.running,
            intercept_tls: !app.intercept_tls,
        }),
        Action::About => NSApplication::sharedApplication(mtm).orderFrontStandardAboutPanel(None),
        Action::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
    }
}
