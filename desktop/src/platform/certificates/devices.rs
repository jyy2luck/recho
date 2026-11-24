use base64::{Engine as _, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    env, fs,
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
    process::{Output, Stdio},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt};

#[derive(Clone)]
pub(crate) struct AndroidDevice {
    pub serial: String,
    pub state: String,
    pub model: String,
}

pub(super) fn android_devices(adb: &Path) -> Result<Vec<AndroidDevice>, String> {
    list_devices(&adb_executable(adb)?)
}

pub(super) fn android_send(adb: &Path, serial: &str, der: &[u8]) -> Result<String, String> {
    let adb = adb_executable(adb)?;
    require_device(&adb, serial)?;
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let certificate = temp.path().join("recho-ca.cer");
    fs::write(&certificate, der).map_err(|error| error.to_string())?;
    let remote = format!("/sdcard/Download/recho-ca-{}.cer", uuid::Uuid::new_v4());
    let mut command = tokio::process::Command::new(adb);
    command
        .args(["-s", serial, "push"])
        .arg(certificate)
        .arg(&remote);
    adb_output(command)?;
    Ok(remote)
}

pub(super) fn android_settings(adb: &Path, serial: &str) -> Result<(), String> {
    let adb = adb_executable(adb)?;
    require_device(&adb, serial)?;
    let open = |action| {
        let mut command = tokio::process::Command::new(&adb);
        command.args(["-s", serial, "shell", "am", "start", "-a", action]);
        let output = adb_output(command)?.to_lowercase();
        if [
            "error:",
            "error type",
            "exception",
            "permission denial",
            "unable to resolve",
            "no activity",
        ]
        .iter()
        .any(|marker| output.contains(marker))
        {
            return Err(truncate(&output));
        }
        Ok(())
    };
    open("android.settings.SECURITY_SETTINGS")
        .or_else(|_| open("android.settings.SETTINGS"))
        .map_err(|error| format!("无法打开手机设置。请在设备上搜索“CA 证书”。{error}"))
}

fn executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return false;
        }
    }
    metadata.is_file()
}

fn sdk_adb(path: PathBuf) -> PathBuf {
    path.join("platform-tools").join(adb_name())
}

fn adb_name() -> &'static str {
    if cfg!(windows) { "adb.exe" } else { "adb" }
}

fn adb_executable(selected: &Path) -> Result<PathBuf, String> {
    if !selected.as_os_str().is_empty() {
        return (selected.is_absolute() && executable(selected))
            .then(|| selected.to_path_buf())
            .ok_or_else(|| "ADB 路径不可用，请选择 Android SDK 中的 adb 可执行文件。".into());
    }
    let sdk = ["ANDROID_SDK_ROOT", "ANDROID_HOME"]
        .into_iter()
        .filter_map(env::var_os)
        .map(|root| sdk_adb(root.into()));
    let home = env::var_os("HOME").map(|home| {
        let relative = if cfg!(target_os = "macos") {
            "Library/Android/sdk"
        } else {
            "Android/Sdk"
        };
        sdk_adb(PathBuf::from(home).join(relative))
    });
    let app_data =
        env::var_os("LOCALAPPDATA").map(|path| sdk_adb(PathBuf::from(path).join("Android/Sdk")));
    let path = env::var_os("PATH").unwrap_or_default();
    sdk.chain(home)
        .chain(app_data)
        .chain(
            env::split_paths(&path)
                .filter(|dir| dir.is_absolute())
                .map(|dir| dir.join(adb_name())),
        )
        .chain([
            PathBuf::from("/opt/homebrew/bin/adb"),
            PathBuf::from("/usr/local/bin/adb"),
        ])
        .find(|path| path.is_absolute() && executable(path))
        .ok_or_else(|| "未找到 ADB。请点击“ADB 路径”选择 adb 可执行文件。".into())
}

fn require_device(adb: &Path, serial: &str) -> Result<(), String> {
    if !valid_serial(serial)
        || !list_devices(adb)?
            .iter()
            .any(|d| d.serial == serial && d.state == "device")
    {
        return Err("设备未连接或尚未授权。请重新加载设备并在手机上允许 USB 调试。".into());
    }
    Ok(())
}

fn list_devices(adb: &Path) -> Result<Vec<AndroidDevice>, String> {
    let mut command = tokio::process::Command::new(adb);
    command.args(["devices", "-l"]);
    parse_devices(&adb_output(command)?)
}

fn parse_devices(output: &str) -> Result<Vec<AndroidDevice>, String> {
    let mut seen = HashSet::new();
    let mut devices = Vec::new();
    let (_, list) = output
        .split_once("List of devices attached")
        .ok_or("ADB 未返回设备列表，请检查所选可执行文件。")?;
    for line in list.lines() {
        let mut fields = line.split_whitespace();
        let (Some(serial), Some(raw_state)) = (fields.next(), fields.next()) else {
            continue;
        };
        let state = match raw_state {
            "no" if fields.next() == Some("permissions") => "no permissions",
            "device" | "offline" | "unauthorized" | "recovery" | "bootloader" | "sideload"
            | "rescue" | "host" | "connecting" => raw_state,
            _ => continue,
        };
        if !valid_serial(serial) || !seen.insert(serial) {
            continue;
        }
        let model = fields
            .find_map(|field| field.strip_prefix("model:"))
            .filter(|value| !value.is_empty())
            .unwrap_or(serial)
            .replace('_', " ");
        devices.push(AndroidDevice {
            serial: serial.into(),
            state: state.into(),
            model,
        });
    }
    Ok(devices)
}

fn valid_serial(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.:[]%".contains(&c))
}

fn adb_output(command: tokio::process::Command) -> Result<String, String> {
    let output = run(command, 64 * 1024)?;
    if !output.status.success() {
        return Err(format!("ADB 操作失败。{}", process_error(&output)));
    }
    Ok(format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
}

pub(super) fn java_check(jdk: &Path, der: &[u8]) -> Result<bool, String> {
    let (keytool, store) = java_paths(jdk)?;
    Ok(java_list(&keytool, &store, der)?.installed)
}

pub(super) fn java_trust(jdk: &Path, der: &[u8]) -> Result<bool, String> {
    let (keytool, store) = java_paths(jdk)?;
    let current = java_list(&keytool, &store, der)?;
    if current.installed {
        return Ok(true);
    }
    if current.alias_exists {
        return Err("Java 信任库中已有同名条目。请检查别名后手动安装。".into());
    }

    let location = store.parent().ok_or("无法定位 Java 信任库目录")?;
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&store)
        .map_err(|error| format!("无法写入 Java 信任库。请复制手动命令安装。{error}"))?;
    file.try_lock()
        .map_err(|error| format!("暂时无法锁定 Java 信任库。请稍后重试。{error}"))?;
    let permissions = file
        .metadata()
        .map_err(|error| error.to_string())?
        .permissions();
    let original = read_store(&file)?;
    let mut stage = tempfile::Builder::new()
        .prefix(".recho-cacerts-")
        .tempfile_in(location)
        .map_err(|error| format!("无法写入 Java 信任库目录。请复制手动命令安装。{error}"))?;
    stage
        .write_all(&original)
        .map_err(|error| error.to_string())?;
    let stage = stage.into_temp_path();
    let staged = java_list(&keytool, stage.as_ref(), der)?;
    if staged.installed {
        return Ok(true);
    }
    if staged.alias_exists {
        return Err("Java 信任库中已有同名条目。请检查别名后手动安装。".into());
    }
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let certificate = temp.path().join("recho-ca.cer");
    fs::write(&certificate, der).map_err(|error| error.to_string())?;
    let mut import = java_import(&keytool, stage.as_ref(), &certificate, der);
    import.args(["-storepass", "changeit", "-noprompt"]);
    java_output(import)?;
    if !java_list(&keytool, stage.as_ref(), der)?.installed {
        return Err("写入后未找到当前 CA 证书。".into());
    }

    fs::set_permissions(&stage, permissions).map_err(|error| error.to_string())?;
    fs::File::open(&stage)
        .and_then(|file| file.sync_all())
        .map_err(|error| error.to_string())?;
    file.rewind().map_err(|error| error.to_string())?;
    let same_store = (|| {
        let opened = same_file::Handle::from_file(file.try_clone()?)?;
        Ok::<_, std::io::Error>(opened == same_file::Handle::from_path(&store)?)
    })()
    .map_err(|error| error.to_string())?;
    if !same_store || read_store(&file)? != original {
        return Err("Java 信任库在安装期间发生变化。请刷新后重试。".into());
    }
    #[cfg(windows)]
    drop(file); // Windows MoveFileEx requires the replaced file to be closed.
    stage
        .persist(&store)
        .map_err(|error| format!("无法更新 Java 信任库。请复制手动命令安装。{error}"))?;
    Ok(true)
}

pub(super) fn java_manual_command(
    jdk: &Path,
    ca_path: &Path,
    der: &[u8],
) -> Result<String, String> {
    let (keytool, store) = java_paths(jdk)?;
    let command = java_import(&keytool, &store, ca_path, der).into_std();
    let arguments = std::iter::once(command.get_program()).chain(command.get_args());
    let quoted: Vec<_> = arguments
        .map(|arg| {
            let arg = arg.to_string_lossy();
            if cfg!(windows) {
                format!("'{}'", arg.replace('\'', "''"))
            } else {
                format!("'{}'", arg.replace('\'', "'\\''"))
            }
        })
        .collect();
    let prefix = if cfg!(windows) { "& " } else { "" };
    Ok(format!("{prefix}{}", quoted.join(" ")))
}

fn java_paths(jdk: &Path) -> Result<(PathBuf, PathBuf), String> {
    let binary = if cfg!(windows) {
        "bin/keytool.exe"
    } else {
        "bin/keytool"
    };
    let home = [
        jdk.to_path_buf(),
        jdk.join("Contents/Home"),
        jdk.join("Home"),
    ]
    .into_iter()
    .find(|folder| folder.is_absolute() && executable(&folder.join(binary)))
    .ok_or("所选 Java 目录中未找到可执行的 bin/keytool。")?;
    let store = ["lib/security/cacerts", "jre/lib/security/cacerts"]
        .into_iter()
        .map(|relative| home.join(relative))
        .find(|path| path.is_file())
        .ok_or("所选 Java 目录中未找到 Java 信任库 cacerts。")?;
    Ok((
        home.join(binary),
        store.canonicalize().map_err(|error| error.to_string())?,
    ))
}

fn read_store(store: &fs::File) -> Result<Vec<u8>, String> {
    let mut data = Vec::new();
    store
        .take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut data)
        .map_err(|error| error.to_string())?;
    if data.len() > 8 * 1024 * 1024 {
        return Err("Java 信任库超过 8 MiB。请使用 keytool 手动安装。".into());
    }
    Ok(data)
}

fn java_import(
    keytool: &Path,
    store: &Path,
    certificate: &Path,
    der: &[u8],
) -> tokio::process::Command {
    let mut command = java_command(keytool);
    command
        .args(["-importcert", "-trustcacerts", "-alias"])
        .arg(java_alias(der))
        .arg("-file")
        .arg(certificate)
        .arg("-keystore")
        .arg(store);
    command
}

fn java_alias(der: &[u8]) -> String {
    format!("recho-{:x}", Sha256::digest(der))
}

fn java_command(keytool: &Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(keytool);
    command
        .env_remove("JAVA_TOOL_OPTIONS")
        .env_remove("JDK_JAVA_OPTIONS")
        .env_remove("_JAVA_OPTIONS")
        .args(["-J-Duser.language=en", "-J-Duser.country=US"]);
    command
}

struct JavaStatus {
    installed: bool,
    alias_exists: bool,
}

fn java_list(keytool: &Path, store: &Path, der: &[u8]) -> Result<JavaStatus, String> {
    let mut command = java_command(keytool);
    command
        .args(["-list", "-rfc", "-keystore"])
        .arg(store)
        .args(["-storepass", "changeit"]);
    let output = java_output(command)?;
    parse_java_list(&output, der)
}

fn parse_java_list(output: &str, der: &[u8]) -> Result<JavaStatus, String> {
    let mut result = JavaStatus {
        installed: false,
        alias_exists: false,
    };
    let alias = java_alias(der);
    let mut entries = output.split("\nAlias name: ");
    let header = entries.next().unwrap_or_default();
    let count = header
        .lines()
        .find_map(|line| {
            line.strip_prefix("Your keystore contains ")?
                .split_whitespace()
                .next()?
                .parse::<usize>()
                .ok()
        })
        .ok_or("无法识别 keytool 返回的 Java 信任库信息。")?;
    let mut read_entries = 0;
    for entry in entries {
        read_entries += 1;
        result.alias_exists |= entry.lines().next() == Some(&alias);
        if !entry
            .lines()
            .any(|line| line == "Entry type: trustedCertEntry")
        {
            continue;
        }
        let (_, pem) = entry
            .split_once("-----BEGIN CERTIFICATE-----")
            .ok_or("信任库证书格式异常")?;
        let (pem, _) = pem
            .split_once("-----END CERTIFICATE-----")
            .ok_or("信任库证书格式异常")?;
        let public: String = pem.chars().filter(|c| !c.is_ascii_whitespace()).collect();
        result.installed |= STANDARD.decode(public).map_err(|error| error.to_string())? == der;
    }
    if count != read_entries {
        return Err("无法读取 Java 信任库条目。".into());
    }
    Ok(result)
}

fn java_output(command: tokio::process::Command) -> Result<String, String> {
    let output = run(command, 8 * 1024 * 1024)?;
    if !output.status.success() {
        let error = process_error(&output);
        let message = error.to_lowercase();
        if message.contains("password") || message.contains("tampered") {
            return Err(
                "无法用默认密码读取或写入 Java 信任库。请复制手动命令并输入实际密码。".into(),
            );
        }
        return Err(format!("Java 信任库操作失败。{error}"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn process_error(output: &Output) -> String {
    let message = if output.stderr.is_empty() {
        &output.stdout
    } else {
        &output.stderr
    };
    truncate(&String::from_utf8_lossy(message))
}

fn truncate(message: &str) -> String {
    message.trim().chars().take(350).collect()
}

fn run(mut command: tokio::process::Command, limit: usize) -> Result<Output, String> {
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| error.to_string())?;
        let stdout = child.stdout.take().ok_or("无法读取命令输出")?;
        let stderr = child.stderr.take().ok_or("无法读取命令错误输出")?;
        tokio::time::timeout(Duration::from_secs(20), async {
            let status = async { child.wait().await.map_err(|error| error.to_string()) };
            let (status, stdout, stderr) =
                tokio::try_join!(status, bounded(stdout, limit), bounded(stderr, limit))?;
            Ok(Output {
                status,
                stdout,
                stderr,
            })
        })
        .await
        .map_err(|_| "证书工具执行超时。".to_string())?
    })
}

async fn bounded(reader: impl AsyncRead + Unpin, limit: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| error.to_string())?;
    if bytes.len() > limit {
        return Err("证书工具输出超过限制。".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_list_parses_models_and_authorization() {
        let devices = parse_devices(
            "List of devices attached\r\n\
            123 device product:sample model:Demo_Phone\r\n\
            [::1]:5555 unauthorized\n\
            denied no permissions\n\
            123 offline\n\
            unsafe;command device\n",
        )
        .unwrap();
        assert_eq!(devices.len(), 3);
        assert_eq!(devices[0].model, "Demo Phone");
        assert_eq!(devices[1].state, "unauthorized");
        assert_eq!(devices[2].state, "no permissions");
        assert!(parse_devices("command output").is_err());
    }

    #[test]
    fn java_store_checks_exact_certificate_and_alias() {
        let der = b"current certificate";
        let certificate = STANDARD.encode(der);
        let output = format!(
            "Your keystore contains 1 entry\r\n\r\nAlias name: {}\r\n\
            Entry type: trustedCertEntry\r\n\r\n-----BEGIN CERTIFICATE-----\r\n\
            {certificate}\r\n-----END CERTIFICATE-----\r\n",
            java_alias(der)
        );
        let current = parse_java_list(&output, der).unwrap();
        assert!(current.installed && current.alias_exists);
        assert!(!parse_java_list(&output, b"another CA").unwrap().installed);
        let private_entry = output.replace("trustedCertEntry", "PrivateKeyEntry");
        let collision = parse_java_list(&private_entry, der).unwrap();
        assert!(!collision.installed && collision.alias_exists);
        assert!(parse_java_list(&output.replace(&certificate, "invalid!"), der).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn java_install_leaves_the_store_intact_if_it_changes_midway() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = tempfile::tempdir().unwrap();
        let jdk = fixture.path();
        fs::create_dir_all(jdk.join("bin")).unwrap();
        fs::create_dir_all(jdk.join("lib/security")).unwrap();
        let store = jdk.join("lib/security/cacerts");
        fs::write(&store, "original").unwrap();
        let keytool = jdk.join("bin/keytool");
        fs::write(&keytool, r#"#!/bin/sh
set -eu
action=$3
while [ "$1" != -keystore ]; do shift; done
store=$2
if [ "$action" = -importcert ]; then
    printf installed > "$store"
    printf 'external update' > "${0%/*}/../lib/security/cacerts"
elif [ "$(cat "$store")" = installed ]; then
    printf 'Your keystore contains 1 entry\n\nAlias name: another\nEntry type: trustedCertEntry\n-----BEGIN CERTIFICATE-----\ndGVzdCBjZXJ0\n-----END CERTIFICATE-----\n'
else
    printf 'Your keystore contains 0 entries\n'
fi
"#).unwrap();
        fs::set_permissions(&keytool, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(!java_check(jdk, b"test cert").unwrap());
        let result = java_trust(jdk, b"test cert").unwrap_err();
        assert!(result.contains("在安装期间发生变化"));
        assert_eq!(fs::read(store).unwrap(), b"external update");
    }

    #[cfg(unix)]
    #[test]
    fn adb_transfers_the_public_file_and_separately_opens_settings() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = tempfile::tempdir().unwrap();
        let adb = fixture.path().join("adb");
        fs::write(
            &adb,
            r#"#!/bin/sh
set -eu
fixture=${0%/*}
printf '%s\n' "$*" >> "$fixture/calls"
if [ "$1" = devices ]; then
    printf 'List of devices attached\nABC device model:Test_Phone\nDEF unauthorized\n'
elif [ "$3" = push ]; then
    cp "$4" "$fixture/public"
    printf '%s' "$4" > "$fixture/source"
elif [ "$7" = android.settings.SECURITY_SETTINGS ]; then
    printf 'Error: unable to resolve Intent\n' >&2
else
    test "$7" = android.settings.SETTINGS
fi
"#,
        )
        .unwrap();
        fs::set_permissions(&adb, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(android_devices(&adb).unwrap().len(), 2);
        assert!(android_send(&adb, "DEF", b"test cert").is_err());
        let remote = android_send(&adb, "ABC", b"test cert").unwrap();
        assert!(remote.starts_with("/sdcard/Download/recho-ca-"));
        assert_eq!(
            fs::read(fixture.path().join("public")).unwrap(),
            b"test cert"
        );
        assert!(!Path::new(&fs::read_to_string(fixture.path().join("source")).unwrap()).exists());
        android_settings(&adb, "ABC").unwrap();
        let calls = fs::read_to_string(fixture.path().join("calls")).unwrap();
        assert_eq!(calls.matches("ABC push").count(), 1);
        assert!(!calls.contains("DEF push"));
        assert!(calls.contains("android.settings.SECURITY_SETTINGS"));
        assert!(calls.contains("android.settings.SETTINGS"));
        assert!(!calls.contains("android.intent.action.VIEW"));
    }
}
