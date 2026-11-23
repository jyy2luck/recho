use eframe::egui;
use std::{path::PathBuf, process::Command};
use tokio::sync::oneshot;

pub type FilePick = oneshot::Receiver<Result<Option<PathBuf>, String>>;

pub fn pick(extension: &'static str, save: bool, ctx: &egui::Context) -> FilePick {
    let (sender, receiver) = oneshot::channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let _ = sender.send(choose(extension, save));
        ctx.request_repaint();
    });
    receiver
}

#[cfg(target_os = "macos")]
fn choose(extension: &str, save: bool) -> Result<Option<PathBuf>, String> {
    let choice = if save {
        format!(r#"choose file name with prompt "导出文件" default name "recho.{extension}""#)
    } else if extension.is_empty() {
        r#"choose file with prompt "打开文件""#.into()
    } else {
        format!(r#"choose file with prompt "打开文件" of type {{"{extension}"}}"#)
    };
    let output = Command::new("/usr/bin/osascript")
        .args([
            "-e",
            "try",
            "-e",
            &format!("POSIX path of ({choice})"),
            "-e",
            "on error number -128",
            "-e",
            r#"return """#,
            "-e",
            "end try",
        ])
        .output()
        .map_err(|error| error.to_string())?;
    chosen_path(output)
}

#[cfg(windows)]
fn choose(extension: &str, save: bool) -> Result<Option<PathBuf>, String> {
    use std::os::windows::process::CommandExt;
    let kind = if save {
        "SaveFileDialog"
    } else {
        "OpenFileDialog"
    };
    let filter = if extension.is_empty() {
        "所有文件 (*.*)|*.*".into()
    } else {
        format!("{extension} (*.{extension})|*.{extension}|所有文件 (*.*)|*.*")
    };
    let script = format!(
        r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
Add-Type -AssemblyName System.Windows.Forms
$d = New-Object System.Windows.Forms.{kind}
$d.Filter = '{filter}'
$d.DefaultExt = '{extension}'
try {{ if ($d.ShowDialog() -eq 'OK') {{ [Console]::WriteLine($d.FileName) }} }} finally {{ $d.Dispose() }}
"#
    );
    let output = Command::new("powershell.exe")
        .creation_flags(0x08000000)
        .args(["-NoProfile", "-STA", "-Command", &script])
        .output()
        .map_err(|error| error.to_string())?;
    chosen_path(output)
}

#[cfg(not(any(target_os = "macos", windows)))]
fn choose(_: &str, _: bool) -> Result<Option<PathBuf>, String> {
    Err("文件选择支持 Windows 和 macOS".into())
}

fn chosen_path(output: std::process::Output) -> Result<Option<PathBuf>, String> {
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().into());
    }
    let path = String::from_utf8(output.stdout).map_err(|error| error.to_string())?;
    let path = path.strip_suffix('\n').unwrap_or(&path);
    let path = path.strip_suffix('\r').unwrap_or(path);
    Ok((!path.is_empty()).then(|| PathBuf::from(path)))
}
