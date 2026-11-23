use eframe::egui;
#[cfg(windows)]
use std::ffi::OsString;
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
};

#[derive(Clone, Copy)]
enum Action {
    Check,
    Trust,
    Untrust,
    Copy,
    Reveal,
}

enum Outcome {
    Status(bool),
    Copied(String),
    Revealed,
}

#[derive(Default)]
pub(crate) struct Certificates {
    status: Option<bool>,
    confirmation: Option<Action>,
    job: Option<Receiver<Result<Outcome, String>>>,
    error: Option<String>,
}

impl Certificates {
    pub(crate) fn show(&mut self, ui: &mut egui::Ui, ca_path: &Path) {
        if let Some(job) = &self.job {
            let result = match job.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err("证书操作未完成".into())),
            };
            if let Some(result) = result {
                self.job = None;
                match result {
                    Ok(Outcome::Status(trusted)) => self.status = Some(trusted),
                    Ok(Outcome::Copied(pem)) => ui.ctx().copy_text(pem),
                    Ok(Outcome::Revealed) => (),
                    Err(error) => self.error = Some(error),
                }
            }
        }
        let ready = !ca_path.as_os_str().is_empty() && self.job.is_none();
        ui.label(match self.status {
            Some(true) => "系统信任检查通过",
            Some(false) => "尚未信任或验证失败",
            None => "尚未检查证书信任",
        });
        ui.add_enabled_ui(ready, |ui| {
            ui.horizontal_wrapped(|ui| {
                for (label, action) in [
                    ("检查信任", Action::Check),
                    ("复制 CA 证书", Action::Copy),
                    ("显示 CA 文件", Action::Reveal),
                ] {
                    if ui.button(label).clicked() {
                        self.start(ui.ctx(), ca_path, action);
                    }
                }
            });
            ui.horizontal_wrapped(|ui| {
                if ui.button("安装并信任 CA").clicked() {
                    self.confirmation = Some(Action::Trust);
                }
                if ui.button("移除当前用户信任").clicked() {
                    self.confirmation = Some(Action::Untrust);
                }
            });
        });
        if self.job.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("正在处理证书操作…");
            });
        }
        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        if let Some(action) = self.confirmation {
            let modal = egui::Modal::new(egui::Id::new("recho_ca_confirmation")).show(ui.ctx(), |ui| {
                ui.set_width(380.0);
                if matches!(action, Action::Trust) {
                    ui.heading("安装并信任本地 CA");
                    ui.label("将 Recho Local CA 加入当前用户的信任设置。信任后，支持系统证书库的客户端可以通过 recho 解密 HTTPS 流量。");
                } else {
                    ui.heading("移除当前用户信任");
                    ui.label("移除当前用户对这张 Recho Local CA 的信任。依赖该信任的 HTTPS 解密可能中断。");
                }
                ui.horizontal(|ui| {
                    let confirmed = ui.button("确认").clicked();
                    (confirmed, ui.button("取消").clicked())
                }).inner
            });
            if modal.inner.0 {
                self.start(ui.ctx(), ca_path, action);
                self.confirmation = None;
            } else if modal.inner.1 || modal.should_close() {
                self.confirmation = None;
            }
        }
    }

    fn start(&mut self, ctx: &egui::Context, ca_path: &Path, action: Action) {
        self.error = None;
        let path = ca_path.to_path_buf();
        let ctx = ctx.clone();
        let (send, receive) = mpsc::sync_channel(1);
        match thread::Builder::new()
            .name("recho-certificate".into())
            .spawn(move || {
                let result = perform(&path, action);
                let _ = send.send(result);
                ctx.request_repaint();
            }) {
            Ok(_) => self.job = Some(receive),
            Err(error) => self.error = Some(error.to_string()),
        }
    }
}

fn perform(path: &Path, action: Action) -> Result<Outcome, String> {
    let mut pem = String::new();
    std::fs::File::open(path)
        .map_err(|error| error.to_string())?
        .take(64 * 1024)
        .read_to_string(&mut pem)
        .map_err(|error| error.to_string())?;
    let pem_text = pem.trim();
    if !pem_text.starts_with("-----BEGIN CERTIFICATE-----")
        || !pem_text.ends_with("-----END CERTIFICATE-----")
        || pem_text.matches("-----BEGIN ").count() != 1
    {
        return Err("本地 CA 文件必须只包含一张公开证书".into());
    }
    match action {
        Action::Copy => return Ok(Outcome::Copied(pem)),
        Action::Reveal => {
            reveal(path)?;
            return Ok(Outcome::Revealed);
        }
        Action::Trust | Action::Untrust => change_trust(path, action)?,
        Action::Check => (),
    }
    trustworthy(path).map(Outcome::Status)
}

fn output(command: &mut Command) -> Result<Output, String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    command
        .stdin(Stdio::null())
        .output()
        .map_err(|error| error.to_string())
}

fn checked(result: Output) -> Result<(), String> {
    if result.status.success() {
        return Ok(());
    }
    let message = if result.stderr.is_empty() {
        &result.stdout
    } else {
        &result.stderr
    };
    let description = String::from_utf8_lossy(message);
    Err(format!(
        "证书操作失败（{}） {}",
        result.status,
        description.trim().chars().take(250).collect::<String>()
    ))
}

#[cfg(target_os = "macos")]
fn trustworthy(path: &Path) -> Result<bool, String> {
    Ok(output(
        Command::new("/usr/bin/security")
            .args(["verify-cert", "-L", "-l", "-q", "-p", "ssl", "-c"])
            .arg(path),
    )?
    .status
    .success())
}

#[cfg(target_os = "macos")]
fn change_trust(path: &Path, action: Action) -> Result<(), String> {
    let mut command = Command::new("/usr/bin/security");
    if matches!(action, Action::Trust) {
        let home = std::env::var_os("HOME").ok_or("无法定位用户钥匙串")?;
        let keychain = PathBuf::from(home).join("Library/Keychains/login.keychain-db");
        command
            .args(["add-trusted-cert", "-r", "trustRoot", "-p", "ssl", "-k"])
            .arg(keychain);
    } else {
        command.arg("remove-trusted-cert");
    }
    checked(output(command.arg(path))?)
}

#[cfg(target_os = "macos")]
fn reveal(path: &Path) -> Result<(), String> {
    checked(output(Command::new("/usr/bin/open").arg("-R").arg(path))?)
}

#[cfg(windows)]
fn executable(name: &str) -> Result<PathBuf, String> {
    std::env::var_os("SystemRoot")
        .map(|path| PathBuf::from(path).join(name))
        .ok_or_else(|| "无法定位 Windows 系统命令".into())
}

#[cfg(windows)]
fn store(path: &Path, operation: &str) -> Result<Output, String> {
    // Paths are passed separately so PowerShell never interprets them as source.
    let mut command = Command::new(executable(
        "System32/WindowsPowerShell/v1.0/powershell.exe",
    )?);
    command
        .args(["-NoProfile", "-NonInteractive", "-Command", operation])
        .env_remove("PSModulePath")
        .env("RECHO_CA_FILE", path);
    output(&mut command)
}

#[cfg(windows)]
fn trustworthy(path: &Path) -> Result<bool, String> {
    let result = store(
        path,
        r"$ErrorActionPreference='Stop';
        $expected=[System.Security.Cryptography.X509Certificates.X509Certificate2]::new($env:RECHO_CA_FILE);
        foreach ($location in @('CurrentUser','LocalMachine')) {
            $store=[System.Security.Cryptography.X509Certificates.X509Store]::new('Root',$location);
            try {
                $store.Open('ReadOnly,OpenExistingOnly');
                foreach ($cert in $store.Certificates) {
                    if ([Convert]::ToBase64String($cert.RawData) -ceq [Convert]::ToBase64String($expected.RawData)) { 'trusted'; exit 0 }
                }
            } finally { $store.Close() }
        }
        'missing'",
    )?;
    if !result.status.success() {
        checked(result)?;
        unreachable!();
    }
    Ok(result.stdout.starts_with(b"trusted"))
}

#[cfg(windows)]
fn change_trust(path: &Path, action: Action) -> Result<(), String> {
    if matches!(action, Action::Trust) {
        return checked(output(
            Command::new(executable("System32/certutil.exe")?)
                .args(["-user", "-addstore", "Root"])
                .arg(path),
        )?);
    }
    checked(store(
        path,
        r"$ErrorActionPreference='Stop';
        $expected=[System.Security.Cryptography.X509Certificates.X509Certificate2]::new($env:RECHO_CA_FILE);
        $store=[System.Security.Cryptography.X509Certificates.X509Store]::new('Root','CurrentUser');
        try {
            $store.Open('ReadWrite,OpenExistingOnly');
            foreach ($cert in $store.Certificates) {
                if ([Convert]::ToBase64String($cert.RawData) -ceq [Convert]::ToBase64String($expected.RawData)) { $store.Remove($cert) }
            }
        } finally { $store.Close() }",
    )?)
}

#[cfg(windows)]
fn reveal(path: &Path) -> Result<(), String> {
    let mut selection = OsString::from("/select,");
    selection.push(path);
    Command::new(executable("explorer.exe")?)
        .arg(selection)
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(not(any(target_os = "macos", windows)))]
fn trustworthy(_: &Path) -> Result<bool, String> {
    Err("当前平台不支持系统证书管理".into())
}

#[cfg(not(any(target_os = "macos", windows)))]
fn change_trust(_: &Path, _: Action) -> Result<(), String> {
    Err("当前平台不支持系统证书管理".into())
}

#[cfg(not(any(target_os = "macos", windows)))]
fn reveal(_: &Path) -> Result<(), String> {
    Err("当前平台不支持显示证书文件".into())
}
