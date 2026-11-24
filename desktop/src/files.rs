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
    let (kind, extension) = match choice {
        Choice::Save(name) => return save_dialog(&name),
        Choice::Open(extension) => ("OpenFileDialog", extension),
        Choice::Directory => ("FolderBrowserDialog", ""),
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
            format!("$d.Filter = '{filter}'\n$d.DefaultExt = '{extension}'"),
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
        .args(["-NoProfile", "-STA", "-Command", &script])
        .output()
        .map_err(|error| error.to_string())?;
    chosen_path(output)
}

#[cfg(windows)]
fn save_dialog(name: &str) -> Result<Option<PathBuf>, String> {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt};
    use windows_sys::Win32::UI::{
        Controls::Dialogs::{
            CommDlgExtendedError, GetSaveFileNameW, OFN_EXPLORER, OFN_HIDEREADONLY,
            OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST, OPENFILENAMEW,
        },
        WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
    };

    let extension = Path::new(name)
        .extension()
        .and_then(|suffix| suffix.to_str())
        .unwrap_or("");
    let all_files = "所有文件 (*.*)\0*.*\0\0";
    let filter = if extension.is_empty() {
        all_files.to_owned()
    } else {
        format!("{extension} (*.{extension})\0*.{extension}\0{all_files}")
    };
    let filter = filter.encode_utf16().collect::<Vec<_>>();
    let default_extension = extension
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let title = "保存文件\0".encode_utf16().collect::<Vec<_>>();
    let mut path = vec![0u16; 32_768];
    for (slot, code_unit) in path.iter_mut().take(32_767).zip(name.encode_utf16()) {
        *slot = code_unit;
    }

    // Keep the dialog attached to the app when its window is in the foreground.
    let foreground = unsafe { GetForegroundWindow() };
    let mut process_id = 0;
    let owner = if !foreground.is_null()
        && unsafe { GetWindowThreadProcessId(foreground, &mut process_id) } != 0
        && process_id == std::process::id()
    {
        foreground
    } else {
        std::ptr::null_mut()
    };
    let mut options = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: filter.as_ptr(),
        nFilterIndex: 1,
        lpstrFile: path.as_mut_ptr(),
        nMaxFile: path.len() as u32,
        lpstrTitle: title.as_ptr(),
        lpstrDefExt: default_extension.as_ptr(),
        Flags: OFN_EXPLORER
            | OFN_HIDEREADONLY
            | OFN_NOCHANGEDIR
            | OFN_OVERWRITEPROMPT
            | OFN_PATHMUSTEXIST,
        ..Default::default()
    };
    // The string buffers remain valid and the output buffer stays writable until the call returns.
    if unsafe { GetSaveFileNameW(&mut options) } != 0 {
        let end = path
            .iter()
            .position(|&code_unit| code_unit == 0)
            .unwrap_or(path.len());
        return Ok(Some(OsString::from_wide(&path[..end]).into()));
    }
    match unsafe { CommDlgExtendedError() } {
        0 => Ok(None),
        error => Err(format!("无法打开保存对话框（0x{error:04X}）")),
    }
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
