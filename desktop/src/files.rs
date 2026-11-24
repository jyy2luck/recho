use eframe::egui;
use std::{
    path::{Path, PathBuf},
    process::Command,
};
use tokio::sync::oneshot;

pub type FilePick = oneshot::Receiver<Result<Option<PathBuf>, String>>;

pub fn pick(extension: &'static str, save: bool, ctx: &egui::Context) -> FilePick {
    if save {
        pick_named(extension, format!("recho.{extension}"), ctx)
    } else {
        spawn_pick(Choice::Open(extension), ctx)
    }
}

pub fn pick_named(extension: &'static str, name: String, ctx: &egui::Context) -> FilePick {
    let name = if Path::new(&name)
        .extension()
        .is_some_and(|suffix| suffix == extension)
    {
        name
    } else {
        format!("{name}.{extension}")
    };
    spawn_pick(Choice::Save(name), ctx)
}

pub fn pick_directory(ctx: &egui::Context) -> FilePick {
    spawn_pick(Choice::Directory, ctx)
}

enum Choice {
    Open(&'static str),
    Save(String),
    Directory,
}

fn spawn_pick(choice: Choice, ctx: &egui::Context) -> FilePick {
    let (sender, receiver) = oneshot::channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let _ = sender.send(choose(choice));
        ctx.request_repaint();
    });
    receiver
}

#[cfg(target_os = "macos")]
fn choose(choice: Choice) -> Result<Option<PathBuf>, String> {
    let (script, default_name) = match choice {
        Choice::Save(name) => (
            r#"choose file name with prompt "导出文件" default name (item 1 of argv)"#.into(),
            name,
        ),
        Choice::Open("") => (
            r#"choose file with prompt "打开文件""#.into(),
            String::new(),
        ),
        Choice::Open(extension) => (
            format!(r#"choose file with prompt "打开文件" of type {{"{extension}"}}"#),
            String::new(),
        ),
        Choice::Directory => (
            r#"choose folder with prompt "选择目录""#.into(),
            String::new(),
        ),
    };
    let output = Command::new("/usr/bin/osascript")
        .args([
            "-e",
            "on run argv",
            "-e",
            "try",
            "-e",
            &format!("POSIX path of ({script})"),
            "-e",
            "on error number -128",
            "-e",
            r#"return """#,
            "-e",
            "end try",
            "-e",
            "end run",
            "--",
            &default_name,
        ])
        .output()
        .map_err(|error| error.to_string())?;
    chosen_path(output)
}

#[cfg(windows)]
fn choose(choice: Choice) -> Result<Option<PathBuf>, String> {
    use std::os::windows::process::CommandExt;
    let (kind, extension, name) = match &choice {
        Choice::Save(name) => (
            "SaveFileDialog",
            Path::new(name)
                .extension()
                .and_then(|suffix| suffix.to_str())
                .unwrap_or(""),
            name.as_str(),
        ),
        Choice::Open(extension) => ("OpenFileDialog", *extension, ""),
        Choice::Directory => ("FolderBrowserDialog", "", ""),
    };
    let (configure, result) = if kind == "FolderBrowserDialog" {
        (String::new(), "SelectedPath")
    } else {
        let filter = if extension.is_empty() {
            "所有文件 (*.*)|*.*".into()
        } else {
            format!("{extension} (*.{extension})|*.{extension}|所有文件 (*.*)|*.*")
        };
        (
            format!(
                "$d.Filter = '{filter}'\n$d.DefaultExt = '{extension}'\n$d.FileName = $env:RECHO_PICK_FILENAME"
            ),
            "FileName",
        )
    };
    let script = format!(
        r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
Add-Type -AssemblyName System.Windows.Forms
$d = New-Object System.Windows.Forms.{kind}
{configure}
try {{ if ($d.ShowDialog() -eq 'OK') {{ [Console]::WriteLine($d.{result}) }} }} finally {{ $d.Dispose() }}
"#
    );
    let output = Command::new("powershell.exe")
        .creation_flags(0x08000000)
        .env_remove("PSModulePath")
        .env("RECHO_PICK_FILENAME", name)
        .args(["-NoProfile", "-STA", "-Command", &script])
        .output()
        .map_err(|error| error.to_string())?;
    chosen_path(output)
}

#[cfg(not(any(target_os = "macos", windows)))]
fn choose(_: Choice) -> Result<Option<PathBuf>, String> {
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
