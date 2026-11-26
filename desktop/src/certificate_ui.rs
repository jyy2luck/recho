use crate::{
    files::{self, FilePick},
    platform::{CERTIFICATE_DOWNLOAD_URL, CertificateAction, CertificateFormat, Certificates},
    theme::{Palette, Theme},
};
use eframe::egui::{self, RichText};
use std::{
    net::Ipv4Addr,
    path::{Path, PathBuf},
};

type Action = CertificateAction;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Page {
    Local,
    Android,
    Ios,
    Firefox,
    Magisk,
    Java,
    Detail,
}

impl Page {
    pub(super) const MENU: [(Self, &str); 7] = [
        (Self::Detail, "查看 CA 证书"),
        (Self::Local, "安装 CA 证书到本机"),
        (Self::Android, "安装 CA 证书到 Android 设备"),
        (Self::Ios, "安装 CA 证书到 iOS 设备"),
        (Self::Firefox, "安装 CA 证书到 Firefox"),
        (Self::Java, "安装 CA 证书到 Java VM"),
        (Self::Magisk, "使用 Magisk 安装 CA 证书"),
    ];
    pub(super) const EXPORTS: [(CertificateFormat, &str); 3] = [
        (CertificateFormat::Der, "导出 CA 公钥 (.crt)"),
        (CertificateFormat::Pem, "导出 CA 公钥 (.pem)"),
        (CertificateFormat::Android, "导出 CA 公钥 (.0)"),
    ];
    const DEVICES: [(Self, &str); 5] = [
        (Self::Android, "Android"),
        (Self::Ios, "iOS"),
        (Self::Firefox, "Firefox"),
        (Self::Magisk, "Magisk"),
        (Self::Java, "Java VM"),
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Local => "安装证书",
            Self::Android => "安装证书到 Android",
            Self::Ios => "安装证书到 iOS",
            Self::Firefox => "安装证书到 Firefox",
            Self::Magisk => "使用 Magisk 安装证书",
            Self::Java => "安装证书到 Java VM",
            Self::Detail => "CA 证书信息",
        }
    }
}

#[derive(Clone, Copy)]
enum Pick {
    Export(CertificateFormat),
    Adb,
    Java,
}

pub(super) struct CertificateUi {
    page: Option<Page>,
    capture_setup: bool,
    return_local: bool,
    manual: bool,
    android_usb: bool,
    qr_open: bool,
    lan: Ipv4Addr,
    adb: PathBuf,
    java: PathBuf,
    picker: Option<(Pick, FilePick)>,
    pending: Option<Action>,
    deferred_export: Option<CertificateFormat>,
    backend: Certificates,
}

impl Default for CertificateUi {
    fn default() -> Self {
        Self {
            page: None,
            capture_setup: false,
            return_local: false,
            manual: false,
            android_usb: false,
            qr_open: false,
            lan: Ipv4Addr::LOCALHOST,
            adb: PathBuf::new(),
            java: PathBuf::new(),
            picker: None,
            pending: None,
            deferred_export: None,
            backend: Certificates::default(),
        }
    }
}

impl CertificateUi {
    pub(super) fn open(&mut self, page: Page) {
        self.capture_setup = false;
        self.manual = false;
        self.return_local = false;
        self.enter(page);
    }

    pub(super) fn open_capture_setup(&mut self) {
        self.open(Page::Local);
        self.capture_setup = true;
    }

    fn enter(&mut self, page: Page) {
        self.page = Some(page);
        self.qr_open = false;
        if matches!(page, Page::Android | Page::Ios) {
            self.lan = recho_core::native::proxy_addresses().0;
        }
        if page == Page::Android {
            self.android_usb = false;
        }
        self.pending = match page {
            Page::Local | Page::Detail => Some(Action::Check),
            Page::Java if !self.java.as_os_str().is_empty() => {
                Some(Action::JavaCheck(self.java.clone()))
            }
            Page::Magisk => Some(Action::Details),
            _ => None,
        };
        self.backend.notice = None;
        self.backend.error = None;
    }

    pub(super) fn export(&mut self, ctx: &egui::Context, format: CertificateFormat) {
        if self.picker.is_some() || self.backend.busy() || self.pending.is_some() {
            return;
        }
        let android_name = self
            .backend
            .detail
            .as_ref()
            .map(|detail| detail.android_name.as_str());
        if format == CertificateFormat::Android && android_name.is_none() {
            self.pending = Some(Action::Details);
            self.deferred_export = Some(format);
            return;
        }
        let picker = files::pick_named(format.extension(), format.filename(android_name), ctx);
        self.picker = Some((Pick::Export(format), picker));
    }

    pub(super) fn poll(&mut self, ctx: &egui::Context, ca: &Path) {
        self.backend.poll(ctx);
        if let Some((kind, picker)) = &mut self.picker {
            let result = match picker.try_recv() {
                Ok(result) => Some(result),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => None,
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    Some(Err("无法打开文件选择器".into()))
                }
            };
            if let Some(result) = result {
                let kind = *kind;
                self.picker = None;
                match result {
                    Ok(Some(path)) => {
                        self.pending = Some(match kind {
                            Pick::Export(format) => Action::Export(path, format),
                            Pick::Adb => {
                                self.adb = path;
                                Action::AndroidDevices(self.adb.clone())
                            }
                            Pick::Java => {
                                self.java = path;
                                Action::JavaCheck(self.java.clone())
                            }
                        })
                    }
                    Ok(None) => (),
                    Err(error) => self.backend.error = Some(error),
                }
            }
        }
        if !ca.as_os_str().is_empty()
            && !self.backend.busy()
            && let Some(action) = self.pending.take()
        {
            self.backend.start(ctx, ca, action);
        }
        if self.pending.is_none()
            && !self.backend.busy()
            && let Some(format) = self.deferred_export.take()
            && self.backend.detail.is_some()
        {
            self.export(ctx, format);
        }
    }

    pub(super) fn take_background_feedback(&mut self) -> Option<Result<String, String>> {
        if self.page.is_some() {
            return None;
        }
        self.backend
            .error
            .take()
            .map(Err)
            .or_else(|| self.backend.notice.take().map(Ok))
    }

    pub(super) fn show(
        &mut self,
        ctx: &egui::Context,
        colors: Palette,
        port: u16,
        running: bool,
        ready: bool,
    ) -> Option<bool> {
        let page = self.page?;
        let capture_setup = self.capture_setup && page == Page::Local;
        let trusted = self.backend.status == Some(true);
        let max_width = if page == Page::Android { 620.0 } else { 565.0 };
        let width = (ctx.content_rect().width() - 54.0).clamp(240.0, max_width);
        let height = (ctx.content_rect().height() - 172.0).clamp(
            160.0,
            match page {
                Page::Local | Page::Java => 319.0,
                Page::Detail | Page::Magisk => 376.0,
                Page::Android | Page::Ios | Page::Firefox => 496.0,
            },
        );
        let enabled =
            ready && self.picker.is_none() && self.pending.is_none() && !self.backend.busy();
        let modal = Theme::modal(ctx, egui::Id::new("certificate_panel")).show(ctx, |ui| {
            ui.set_width(width);
            ui.horizontal(|ui| {
                ui.heading(if capture_setup {
                    "首次 HTTPS 调试"
                } else {
                    page.title()
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if page == Page::Local {
                        if ui.selectable_label(self.manual, "手动").clicked() {
                            self.manual = true;
                        }
                        if ui.selectable_label(!self.manual, "自动").clicked() && self.manual {
                            self.manual = false;
                            self.pending = Some(Action::Check);
                        }
                    }
                });
            });
            if capture_setup {
                ui.label("信任 Recho CA 后，调试时默认解密经过 Recho 的 HTTPS 流量。");
            }
            ui.add_space(6.0);
            ui.separator();
            egui::ScrollArea::vertical()
                .id_salt((page.title(), self.manual))
                .max_height(height)
                .auto_shrink([false, page == Page::Android])
                .show(ui, |ui| {
                    ui.add_space(9.0);
                    ui.add_enabled_ui(enabled, |ui| match page {
                        Page::Local if self.manual => self.local_manual(ui),
                        Page::Local => self.local_auto(ui, colors),
                        Page::Android => self.android(ui, colors, port, running),
                        Page::Ios => self.ios(ui, port, running),
                        Page::Firefox => self.firefox(ui),
                        Page::Magisk => self.magisk(ui),
                        Page::Java => self.java(ui, colors),
                        Page::Detail => self.details(ui, colors),
                    });
                });
            ui.separator();
            ui.horizontal(|ui| {
                if let Some(error) = &self.backend.error {
                    ui.colored_label(colors.red, error);
                } else if let Some(notice) = &self.backend.notice {
                    ui.colored_label(colors.green, notice);
                } else if self.picker.is_some() || !enabled {
                    ui.spinner();
                    ui.weak("正在处理…");
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = if self.return_local {
                    "返回"
                } else if capture_setup && !trusted {
                    "暂不解密"
                } else if page == Page::Detail {
                    "关闭"
                } else {
                    "完成"
                };
                ui.add_enabled(
                    self.picker.is_none() && (!capture_setup || enabled),
                    egui::Button::new(label),
                )
                .clicked()
            })
            .inner
        });
        if self.picker.is_none() && (modal.inner || modal.should_close()) {
            if self.qr_open && !modal.inner {
                self.qr_open = false;
            } else if self.return_local {
                self.return_local = false;
                self.enter(Page::Local);
            } else {
                self.page = None;
                if std::mem::take(&mut self.capture_setup) {
                    return Some(modal.inner && trusted);
                }
            }
        }
        None
    }

    fn local_auto(&mut self, ui: &mut egui::Ui, colors: Palette) {
        ui.add_space(23.0);
        ui.vertical_centered(|ui| {
            let (message, color) = local_status(&self.backend, colors);
            ui.label(RichText::new(message).size(20.0).strong().color(color));
            ui.weak(if cfg!(target_os = "macos") {
                "当前用户登录钥匙串 · SSL 信任"
            } else {
                "当前用户证书存储 · SSL 信任"
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let trusted = self.backend.status == Some(true);
                let label = if trusted {
                    "移除信任"
                } else {
                    "现在安装"
                };
                if ui
                    .add(egui::Button::new(label).fill(colors.selected))
                    .clicked()
                {
                    self.pending = Some(if trusted {
                        Action::Untrust
                    } else {
                        Action::Trust
                    });
                }
                if ui.button("重新检查").clicked() {
                    self.pending = Some(Action::Check);
                }
                if ui.button("查看 CA 证书").clicked() {
                    self.return_local = true;
                    self.enter(Page::Detail);
                }
            });
        });
        ui.add_space(20.0);
        ui.separator();
        ui.label(RichText::new("其他设备和应用").strong());
        ui.horizontal_wrapped(|ui| {
            for (page, label) in Page::DEVICES {
                if ui
                    .add_sized([89.0, 36.0], egui::Button::new(label))
                    .clicked()
                {
                    self.return_local = true;
                    self.enter(page);
                }
            }
        });
    }

    fn local_manual(&mut self, ui: &mut egui::Ui) {
        step(ui, "1. 导出 CA 证书", "保存 Recho 的公开根证书。");
        self.export_button(ui, "导出 .crt", CertificateFormat::Der);
        if cfg!(target_os = "macos") {
            step(
                ui,
                "2. 导入到登录钥匙串",
                "在钥匙串访问中选中「登录」，从「文件 → 导入项目」添加证书，也可以把 .crt 拖入证书列表。",
            );
            if ui.button("打开钥匙串访问").clicked() {
                self.pending = Some(Action::OpenKeychain);
            }
            step(
                ui,
                "3. 设置 SSL 信任",
                "打开刚导入的 CA，展开「信任」，将 SSL 设为「始终信任」，关闭窗口并按系统提示确认。",
            );
        } else {
            step(
                ui,
                "2. 将证书导入当前用户",
                "打开 .crt，选择「安装证书 → 当前用户 → 将所有证书放入下列存储」，然后选择「受信任的根证书颁发机构」。",
            );
        }
        if ui.button("重新检查信任").clicked() {
            self.pending = Some(Action::Check);
        }
    }

    fn android(&mut self, ui: &mut egui::Ui, colors: Palette, port: u16, running: bool) {
        ui.strong("1. 获取 CA 证书");
        ui.horizontal_wrapped(|ui| {
            if ui
                .add(
                    egui::Button::selectable(!self.android_usb, "Wi-Fi 下载")
                        .min_size(egui::vec2(92.0, 30.0)),
                )
                .clicked()
                && self.android_usb
            {
                self.android_usb = false;
                self.backend.error = None;
                self.backend.notice = None;
                self.lan = recho_core::native::proxy_addresses().0;
            }
            if ui
                .add(
                    egui::Button::selectable(self.android_usb, "USB / ADB")
                        .min_size(egui::vec2(92.0, 30.0)),
                )
                .clicked()
                && !self.android_usb
            {
                self.android_usb = true;
                self.pending = Some(Action::AndroidDevices(self.adb.clone()));
            }
            self.export_button(ui, "导出 .crt", CertificateFormat::Der);
        });
        ui.add_space(5.0);
        let width = ui.available_width();
        let card = egui::Frame::new()
            .fill(colors.bg)
            .stroke(egui::Stroke::new(1.0, colors.line))
            .corner_radius(6)
            .inner_margin(egui::Margin::same(12));
        card.show(ui, |ui| {
            ui.set_width(width - card.total_margin().sum().x);
            if self.android_usb {
                self.android_adb(ui, colors);
            } else {
                self.android_wifi(ui, port, running);
            }
        });
        step(
            ui,
            "2. 在手机上安装",
            "在 Android 设置中打开「安全 → 加密与凭据 → 安装证书 → CA 证书」，选择下载或发送到手机的证书。菜单名称可能因系统版本而异。",
        );
    }

    fn android_wifi(&self, ui: &mut egui::Ui, port: u16, running: bool) {
        let has_address = !self.lan.is_loopback() && port != 0;
        let can_download = has_address && running;
        let width = ui.available_width();
        let beside = can_download && width >= 430.0;
        let qr_width = 148.0;
        let text_width = if beside {
            width - qr_width - ui.spacing().item_spacing.x
        } else {
            width
        };
        let layout = if beside {
            egui::Layout::left_to_right(egui::Align::Min)
        } else {
            egui::Layout::top_down(egui::Align::Min)
        };
        ui.with_layout(layout, |ui| {
            ui.vertical(|ui| {
                ui.set_width(text_width);
                ui.strong("配置 Wi-Fi 代理");
                ui.label("将手机与电脑连到同一网络，在手机 Wi-Fi 设置中选择「手动代理」。");
                if has_address {
                    copy_row(ui, "主机", &self.lan.to_string());
                    copy_row(ui, "端口", &port.to_string());
                } else {
                    ui.weak(
                        "未获取到局域网代理地址。连接网络后重新打开此页，或通过 USB 获取证书。",
                    );
                }
                if can_download {
                    step(
                        ui,
                        "下载 CA 证书",
                        "保存代理设置后，在手机浏览器扫码或打开下载地址。",
                    );
                    copy_row(ui, "下载地址", CERTIFICATE_DOWNLOAD_URL);
                } else if has_address {
                    ui.weak("启动调试后即可扫码下载证书。");
                }
            });
            if can_download {
                ui.vertical_centered(|ui| {
                    ui.set_width(qr_width.min(width));
                    show_qr(ui);
                });
            }
        });
    }

    fn android_adb(&mut self, ui: &mut egui::Ui, colors: Palette) {
        ui.strong("通过 USB 发送证书");
        ui.label("用 USB 连接手机，开启 USB 调试，并在手机上批准调试授权。");
        ui.horizontal_wrapped(|ui| {
            if ui.button("刷新设备").clicked() {
                self.pending = Some(Action::AndroidDevices(self.adb.clone()));
            }
            if ui.button("ADB 路径…").clicked() {
                self.picker = Some((Pick::Adb, files::pick("", false, ui.ctx())));
            }
            ui.weak(if self.adb.as_os_str().is_empty() {
                "自动查找 ADB".into()
            } else {
                self.adb.display().to_string()
            });
        });
        ui.add_space(12.0);
        if self.backend.devices.is_empty() {
            ui.colored_label(colors.muted, "未发现连接的设备");
        }
        for device in &self.backend.devices {
            ui.group(|ui| {
                ui.label(RichText::new(&device.model).strong());
                let status = match device.state.as_str() {
                    "device" => "已连接",
                    "unauthorized" => "请在设备上授权 USB 调试",
                    "offline" => "离线",
                    state => state,
                };
                ui.weak(format!("{} · {}", device.serial, status));
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(device.state == "device", egui::Button::new("发送 CA 证书"))
                        .clicked()
                    {
                        self.pending =
                            Some(Action::AndroidSend(self.adb.clone(), device.serial.clone()));
                    }
                    if ui
                        .add_enabled(
                            device.state == "device",
                            egui::Button::new("打开设备安全设置"),
                        )
                        .clicked()
                    {
                        self.pending = Some(Action::AndroidSettings(
                            self.adb.clone(),
                            device.serial.clone(),
                        ));
                    }
                });
            });
        }
    }

    fn ios(&mut self, ui: &mut egui::Ui, port: u16, running: bool) {
        step(
            ui,
            "1. 配置 Wi-Fi 代理",
            "将 iOS 设备与电脑连到同一网络，在 Wi-Fi 详情中打开「配置代理 → 手动」。",
        );
        copy_row(ui, "服务器", &self.lan.to_string());
        copy_row(ui, "端口", &port.to_string());
        if self.lan.is_loopback() || !running {
            ui.weak("通过手机安装前，请确保检测到局域网 IPv4 地址，并启动调试代理。");
        }
        step(
            ui,
            "2. 下载 CA 证书",
            "在 iOS 设备的 Safari 中访问下载地址，允许下载描述文件。",
        );
        copy_row(ui, "下载地址", CERTIFICATE_DOWNLOAD_URL);
        if ui.selectable_label(self.qr_open, "下载二维码").clicked() {
            self.qr_open = !self.qr_open;
        }
        if self.qr_open {
            show_qr(ui);
        }
        self.export_button(ui, "手动导出 .crt", CertificateFormat::Der);
        step(
            ui,
            "3. 安装描述文件",
            "打开「设置 → 通用 → VPN 与设备管理」，选中下载的描述文件并安装。",
        );
        step(
            ui,
            "4. 启用完全信任",
            "打开「设置 → 通用 → 关于本机 → 证书信任设置」，为 Recho CA 启用完全信任。",
        );
    }

    fn firefox(&mut self, ui: &mut egui::Ui) {
        step(
            ui,
            "1. 导出 CA 证书",
            "可以将 Recho CA 导入 Firefox 的证书颁发机构列表。",
        );
        self.export_button(ui, "导出 .crt", CertificateFormat::Der);
        step(
            ui,
            "2. 在 Firefox 中导入",
            "打开「设置 → 隐私与安全 → 证书 → 查看证书 → 证书颁发机构 → 导入」，选择导出的 .crt。",
        );
        copy_row(ui, "设置地址", "about:preferences#privacy");
        step(
            ui,
            "3. 信任网站证书",
            "在 Firefox 的确认框中勾选「信任此 CA 来标识网站」，然后保存。",
        );
    }

    fn magisk(&mut self, ui: &mut egui::Ui) {
        step(
            ui,
            "1. 下载模块",
            "模块包含此 Recho 实例的公开 CA，适用于传统系统证书目录的 Android 7–13 Magisk 设备。",
        );
        self.export_button(ui, "导出 Magisk 模块 (.zip)", CertificateFormat::Magisk);
        if let Some(detail) = &self.backend.detail {
            ui.weak(format!("证书文件 {}", detail.android_name));
        }
        step(
            ui,
            "2. 在 Magisk 中安装",
            "在设备上打开 Magisk 的「模块 → 从本地安装」，选择刚导出的 ZIP，安装完成后重启设备。",
        );
    }

    fn java(&mut self, ui: &mut egui::Ui, colors: Palette) {
        let (label, color) = match self.backend.java_status {
            Some(true) => ("CA 已安装到此 JDK", colors.green),
            Some(false) => ("此 JDK 尚未安装 CA", colors.warning),
            None => ("未知证书安装状态", colors.muted),
        };
        ui.label(RichText::new(label).size(18.0).strong().color(color));
        step(
            ui,
            "Java 安装路径",
            "选择 JVM 实际使用的 JDK，Recho 会检查该 JDK 的 cacerts。",
        );
        if ui.button("选择 JDK 目录…").clicked() {
            self.picker = Some((Pick::Java, files::pick_directory(ui.ctx())));
        }
        if !self.java.as_os_str().is_empty() {
            ui.monospace(self.java.display().to_string());
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(
                        self.backend.java_status == Some(false),
                        egui::Button::new("现在安装"),
                    )
                    .clicked()
                {
                    self.pending = Some(Action::JavaTrust(self.java.clone()));
                }
                if ui.button("重新检查").clicked() {
                    self.pending = Some(Action::JavaCheck(self.java.clone()));
                }
                if ui.button("复制手动安装命令").clicked() {
                    self.pending = Some(Action::JavaManual(self.java.clone()));
                }
            });
        }
    }

    fn details(&mut self, ui: &mut egui::Ui, colors: Palette) {
        let Some(detail) = &self.backend.detail else {
            ui.weak("等待本地 CA 证书");
            return;
        };
        egui::Grid::new("certificate_metadata")
            .num_columns(2)
            .spacing([22.0, 12.0])
            .show(ui, |ui| {
                for (label, value) in [
                    ("主题", &detail.subject),
                    ("签发者", &detail.issuer),
                    ("序列号", &detail.serial),
                    ("有效期起始", &detail.not_before),
                    ("有效期截止", &detail.not_after),
                    ("SHA-256", &detail.sha256),
                    ("SHA-1", &detail.sha1),
                    ("Android 文件名", &detail.android_name),
                ] {
                    ui.strong(label);
                    ui.label(value);
                    ui.end_row();
                }
            });
        ui.add_space(9.0);
        let (label, color) = local_status(&self.backend, colors);
        ui.colored_label(color, label);
        ui.horizontal_wrapped(|ui| {
            if ui.button("复制 PEM").clicked() {
                self.pending = Some(Action::Copy);
            }
            if ui.button("在文件管理器中显示").clicked() {
                self.pending = Some(Action::Reveal);
            }
            self.export_button(ui, "导出 .crt", CertificateFormat::Der);
            self.export_button(ui, "导出 .pem", CertificateFormat::Pem);
            self.export_button(ui, "导出 .0", CertificateFormat::Android);
        });
    }

    fn export_button(&mut self, ui: &mut egui::Ui, label: &str, format: CertificateFormat) {
        if ui.button(label).clicked() {
            self.export(ui.ctx(), format);
        }
    }
}

fn local_status(backend: &Certificates, colors: Palette) -> (&'static str, egui::Color32) {
    match (backend.status, backend.installed) {
        (Some(true), _) => ("证书已安装并受信任", colors.green),
        (_, Some(true)) => ("证书已安装，尚未取得 SSL 信任", colors.warning),
        (Some(false), _) => ("证书未安装", colors.warning),
        _ => ("证书安装状态未知", colors.muted),
    }
}

fn step(ui: &mut egui::Ui, title: &str, text: &str) {
    ui.add_space(11.0);
    ui.strong(title);
    ui.label(text);
}

fn copy_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.label(label);
        ui.monospace(value);
        if ui.button("复制").clicked() {
            ui.ctx().copy_text(value.to_owned());
        }
    });
}

fn show_qr(ui: &mut egui::Ui) {
    let Ok(code) = qrcode::QrCode::new(CERTIFICATE_DOWNLOAD_URL) else {
        return;
    };
    let scale = 4.0;
    let margin = 4.0;
    let size = (code.width() as f32 + margin * 2.0) * scale;
    let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(size), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, egui::Color32::WHITE);
    let origin = rect.min + egui::Vec2::splat(margin * scale);
    for (index, color) in code.to_colors().into_iter().enumerate() {
        if color == qrcode::Color::Dark {
            let point = egui::vec2((index % code.width()) as f32, (index / code.width()) as f32);
            let module =
                egui::Rect::from_min_size(origin + point * scale, egui::Vec2::splat(scale));
            ui.painter().rect_filled(module, 0.0, egui::Color32::BLACK);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn android_opens_on_wifi_without_requiring_adb() {
        let adb = PathBuf::from("platform-tools").join("adb");
        let mut certificates = CertificateUi {
            adb: adb.clone(),
            android_usb: true,
            pending: Some(Action::AndroidDevices(adb.clone())),
            ..CertificateUi::default()
        };

        certificates.open(Page::Android);

        assert!(!certificates.android_usb);
        assert!(certificates.pending.is_none());
        assert_eq!(certificates.adb, adb);
    }
}
