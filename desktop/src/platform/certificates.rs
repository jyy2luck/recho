mod devices;
mod exports;

use eframe::egui;
use recho_core::native::PublicCertificate;
use sha2::Digest as _;
#[cfg(windows)]
use std::ffi::OsString;
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
};

use devices::AndroidDevice;
pub(crate) use exports::CertificateFormat;
pub(crate) use recho_core::native::CERTIFICATE_DOWNLOAD_URL;

pub(crate) enum CertificateAction {
    Check,
    Trust,
    Untrust,
    Copy,
    Reveal,
    OpenKeychain,
    Details,
    Export(PathBuf, CertificateFormat),
    AndroidDevices(PathBuf),
    AndroidSend(PathBuf, String),
    AndroidSettings(PathBuf, String),
    JavaCheck(PathBuf),
    JavaTrust(PathBuf),
    JavaManual(PathBuf),
}

pub(crate) struct CertificateDetail {
    pub subject: String,
    pub issuer: String,
    pub serial: String,
    pub not_before: String,
    pub not_after: String,
    pub sha1: String,
    pub sha256: String,
    pub android_name: String,
}

impl From<PublicCertificate> for CertificateDetail {
    fn from(cert: PublicCertificate) -> Self {
        let android_name = exports::android_name(&cert);
        Self {
            subject: cert.subject,
            issuer: cert.issuer,
            serial: cert.serial,
            not_before: cert.not_before,
            not_after: cert.not_after,
            sha1: fingerprint(sha1::Sha1::digest(&cert.der)),
            sha256: fingerprint(sha2::Sha256::digest(&cert.der)),
            android_name,
        }
    }
}

fn fingerprint(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

enum Outcome {
    Status {
        detail: CertificateDetail,
        status: Option<(bool, bool)>,
        error: Option<String>,
    },
    Copied(String),
    Details(CertificateDetail),
    Devices(Vec<AndroidDevice>),
    Java(bool),
    Notice(String),
    Revealed,
}

#[derive(Default)]
pub(crate) struct Certificates {
    pub status: Option<bool>,
    pub installed: Option<bool>,
    pub detail: Option<CertificateDetail>,
    pub devices: Vec<AndroidDevice>,
    pub java_status: Option<bool>,
    pub notice: Option<String>,
    pub error: Option<String>,
    job: Option<Receiver<Result<Outcome, String>>>,
}

impl Certificates {
    pub(crate) fn poll(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.job else {
            return;
        };
        let result = match job.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("证书操作未完成".into()),
        };
        self.job = None;
        match result {
            Ok(Outcome::Status {
                detail,
                status,
                error,
            }) => {
                self.detail = Some(detail);
                self.installed = status.map(|(installed, _)| installed);
                self.status = status.map(|(_, trusted)| trusted);
                self.error = error;
            }
            Ok(Outcome::Copied(text)) => ctx.copy_text(text),
            Ok(Outcome::Details(detail)) => self.detail = Some(detail),
            Ok(Outcome::Devices(devices)) => self.devices = devices,
            Ok(Outcome::Java(trusted)) => self.java_status = Some(trusted),
            Ok(Outcome::Notice(notice)) => self.notice = Some(notice),
            Ok(Outcome::Revealed) => (),
            Err(error) => self.error = Some(error),
        }
    }

    pub(crate) fn busy(&self) -> bool {
        self.job.is_some()
    }

    pub(crate) fn start(&mut self, ctx: &egui::Context, ca_path: &Path, action: CertificateAction) {
        if self.busy() {
            return;
        }
        self.error = None;
        self.notice = None;
        use CertificateAction::*;
        match action {
            Check | Trust | Untrust => {
                self.installed = None;
                self.status = None;
            }
            AndroidDevices(_) => self.devices.clear(),
            JavaCheck(_) | JavaTrust(_) => self.java_status = None,
            _ => (),
        }
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

fn perform(path: &Path, action: CertificateAction) -> Result<Outcome, String> {
    use CertificateAction::*;
    match action {
        AndroidDevices(adb) => return devices::android_devices(&adb).map(Outcome::Devices),
        AndroidSettings(adb, serial) => {
            devices::android_settings(&adb, &serial)?;
            return Ok(Outcome::Notice("已在设备上打开安全设置".into()));
        }
        OpenKeychain => {
            open_keychain()?;
            return Ok(Outcome::Revealed);
        }
        Reveal => {
            reveal(path)?;
            return Ok(Outcome::Revealed);
        }
        _ => (),
    }
    let cert = PublicCertificate::read(path)
        .map_err(|error| format!("无法读取本地 CA 公钥证书：{error}"))?;
    match action {
        Copy => Ok(Outcome::Copied(cert.pem)),
        Details => Ok(Outcome::Details(cert.into())),
        Check | Trust | Untrust => {
            let mut snapshot = tempfile::Builder::new()
                .suffix(".crt")
                .tempfile()
                .map_err(|error| error.to_string())?;
            snapshot
                .write_all(&cert.der)
                .map_err(|error| error.to_string())?;
            let error = match action {
                Trust => change_trust(snapshot.path(), true).err(),
                Untrust => change_trust(snapshot.path(), false).err(),
                _ => None,
            };
            let (status, error) = match trust_status(snapshot.path(), &cert.der) {
                Ok(status) => (
                    Some(status),
                    error.or_else(|| {
                        (matches!(action, Trust) && !status.1)
                            .then(|| "系统尚未确认信任，请检查证书管理器中的设置。".into())
                    }),
                ),
                Err(check_error) => (None, error.or(Some(check_error))),
            };
            Ok(Outcome::Status {
                detail: cert.into(),
                status,
                error,
            })
        }
        Export(destination, format) => {
            exports::export(path, &destination, format, &cert)?;
            Ok(Outcome::Notice(format!(
                "已导出到 {}",
                destination.display()
            )))
        }
        AndroidSend(adb, serial) => devices::android_send(&adb, &serial, &cert.der).map(|file| {
            Outcome::Notice(format!(
                "已发送证书到 {file}，请在设备的安全设置中选择该文件。"
            ))
        }),
        JavaCheck(jdk) => devices::java_check(&jdk, &cert.der).map(Outcome::Java),
        JavaTrust(jdk) => devices::java_trust(&jdk, &cert.der).map(Outcome::Java),
        JavaManual(jdk) => devices::java_manual_command(&jdk, path, &cert.der).map(Outcome::Copied),
        AndroidDevices(_) | AndroidSettings(_, _) | OpenKeychain | Reveal => unreachable!(),
    }
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
fn trust_status(path: &Path, der: &[u8]) -> Result<(bool, bool), String> {
    let trusted = output(
        Command::new("/usr/bin/security")
            .args(["verify-cert", "-L", "-l", "-q", "-p", "ssl", "-c"])
            .arg(path),
    )?
    .status
    .success();
    if trusted {
        return Ok((true, true));
    }
    let fingerprint = fingerprint(sha2::Sha256::digest(der)).replace(':', "");
    let result = output(Command::new("/usr/bin/security").args(["find-certificate", "-a", "-Z"]))?;
    if !result.status.success() {
        checked(result)?;
        unreachable!();
    }
    let installed = String::from_utf8_lossy(&result.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix("SHA-256 hash: "))
        .any(|hash| hash.trim().eq_ignore_ascii_case(&fingerprint));
    Ok((installed, false))
}

#[cfg(target_os = "macos")]
fn change_trust(path: &Path, trust: bool) -> Result<(), String> {
    let mut command = Command::new("/usr/bin/security");
    if trust {
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
fn open_keychain() -> Result<(), String> {
    checked(output(
        Command::new("/usr/bin/open").args(["-b", "com.apple.keychainaccess"]),
    )?)
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
fn trust_status(path: &Path, _: &[u8]) -> Result<(bool, bool), String> {
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
    let trusted = result.stdout.starts_with(b"trusted");
    Ok((trusted, trusted))
}

#[cfg(windows)]
fn change_trust(path: &Path, trust: bool) -> Result<(), String> {
    if trust {
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
fn open_keychain() -> Result<(), String> {
    Command::new(executable("System32/mmc.exe")?)
        .arg(executable("System32/certmgr.msc")?)
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
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
fn trust_status(_: &Path, _: &[u8]) -> Result<(bool, bool), String> {
    Err("当前平台不支持系统证书管理".into())
}

#[cfg(not(any(target_os = "macos", windows)))]
fn change_trust(_: &Path, _: bool) -> Result<(), String> {
    Err("当前平台不支持系统证书管理".into())
}

#[cfg(not(any(target_os = "macos", windows)))]
fn open_keychain() -> Result<(), String> {
    Err("当前平台不支持系统证书管理".into())
}

#[cfg(not(any(target_os = "macos", windows)))]
fn reveal(_: &Path) -> Result<(), String> {
    Err("当前平台不支持显示证书文件".into())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use recho_core::native::DesktopEngine;

    #[tokio::test]
    async fn a_new_local_ca_is_readable_and_not_in_the_keychain() {
        let temp = tempfile::tempdir().unwrap();
        let engine = DesktopEngine::start(temp.path().join("data"), Some(0))
            .await
            .unwrap();
        let result = perform(&engine.ca_path(), CertificateAction::Check).unwrap();
        assert!(matches!(result, Outcome::Status {
            detail,
            status: Some((false, false)),
            error: None,
        } if detail.subject.contains("Recho Local CA")
            && detail.sha1.len() == 59
            && detail.sha256.len() == 95));
        engine.stop().await;
    }
}
