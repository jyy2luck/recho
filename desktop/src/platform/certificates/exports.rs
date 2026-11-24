use recho_core::native::{PublicCertificate, write_local_file};
use sha2::Digest as _;
use std::{
    fs,
    io::{Cursor, Write},
    path::Path,
};
use zip::{ZipWriter, write::SimpleFileOptions};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CertificateFormat {
    Der,
    Pem,
    Android,
    Magisk,
}

impl CertificateFormat {
    pub(crate) fn extension(self) -> &'static str {
        match self {
            Self::Der => "crt",
            Self::Pem => "pem",
            Self::Android => "0",
            Self::Magisk => "zip",
        }
    }

    pub(crate) fn filename(self, android_name: Option<&str>) -> String {
        match self {
            Self::Android => android_name.unwrap_or("recho-ca.0").into(),
            Self::Magisk => "recho-magisk-ca.zip".into(),
            _ => format!("recho-ca.{}", self.extension()),
        }
    }
}

pub(super) fn android_name(cert: &PublicCertificate) -> String {
    let digest = md5::Md5::digest(&cert.subject_der);
    let hash = u32::from_le_bytes(digest[..4].try_into().unwrap());
    format!("{hash:08x}.0")
}

pub(super) fn export(
    source: &Path,
    destination: &Path,
    format: CertificateFormat,
    cert: &PublicCertificate,
) -> Result<(), String> {
    let canonical_source = fs::canonicalize(source).map_err(|error| error.to_string())?;
    if let Ok(target) = fs::canonicalize(destination) {
        let key = canonical_source.with_file_name("ca.key");
        for protected in [
            source,
            &canonical_source,
            &source.with_file_name("ca.key"),
            &key,
        ] {
            if target == protected || same_file::is_same_file(&target, protected).unwrap_or(false) {
                return Err("请选择其他位置，当前 CA 证书和私钥文件受保护。".into());
            }
        }
    }
    match format {
        CertificateFormat::Der => write_local_file(destination, &cert.der),
        CertificateFormat::Pem | CertificateFormat::Android => {
            write_local_file(destination, cert.pem.as_bytes())
        }
        CertificateFormat::Magisk => write_local_file(
            destination,
            &magisk(cert).map_err(|error| error.to_string())?,
        ),
    }
}

fn magisk(cert: &PublicCertificate) -> zip::result::ZipResult<Vec<u8>> {
    let certificate_path = format!("system/etc/security/cacerts/{}", android_name(cert));
    let files: [(&str, &str, u32); 5] = [
        ("module.prop", MODULE_PROPERTIES, 0o644),
        ("customize.sh", CUSTOMIZE, 0o755),
        (
            "META-INF/com/google/android/update-binary",
            INSTALLER,
            0o755,
        ),
        (
            "META-INF/com/google/android/updater-script",
            "#MAGISK\n",
            0o644,
        ),
        (&certificate_path, &cert.pem, 0o644),
    ];
    let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
    for (path, content, mode) in files {
        archive.start_file(path, SimpleFileOptions::default().unix_permissions(mode))?;
        archive.write_all(content.as_bytes())?;
    }
    Ok(archive.finish()?.into_inner())
}

const MODULE_PROPERTIES: &str = concat!(
    "id=recho_ca\n",
    "name=Recho Local CA (Android 7–13)\n",
    "version=",
    env!("CARGO_PKG_VERSION"),
    "\n",
    "versionCode=1\n",
    "author=Recho\n",
    "description=Public CA overlay for Android API 24–33 classic system certificate stores. Reboot after installation.\n",
);

const CUSTOMIZE: &str = r#"#!/system/bin/sh
SKIPUNZIP=0
case "$API" in
  ''|*[!0-9]*) abort "Cannot determine Android API level." ;;
esac
[ "$API" -ge 24 ] && [ "$API" -le 33 ] || abort "This module requires Android API 24–33."
if [ -d /apex/com.android.conscrypt/cacerts ] || [ -d /apex/com.android.conscrypt/etc/security/cacerts ]; then
  abort "This module requires a classic system CA store."
fi
[ -d /system/etc/security/cacerts ] || abort "System CA store unavailable."
set_perm_recursive "$MODPATH/system" 0 0 0755 0644
ui_print "Reboot to make the Recho public CA available."
"#;

const INSTALLER: &str = r#"#!/sbin/sh
umask 022
OUTFD=$2
ZIPFILE=$3
if [ ! -f /data/adb/magisk/util_functions.sh ]; then
  echo "! Install this ZIP through Magisk."
  exit 1
fi
. /data/adb/magisk/util_functions.sh
case "$MAGISK_VER_CODE" in
  ''|*[!0-9]*) ui_print "! Unable to detect Magisk version."; exit 1 ;;
esac
[ "$MAGISK_VER_CODE" -ge 20400 ] || abort "Magisk 20.4 or newer is required."
install_module
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use recho_core::native::DesktopEngine;
    use std::io::Read;

    #[tokio::test]
    async fn exports_matching_public_certificate_and_preserves_ca_files() {
        let temp = tempfile::tempdir().unwrap();
        let engine = DesktopEngine::start(temp.path().join("data"), Some(0))
            .await
            .unwrap();
        let source = engine.ca_path();
        let cert = PublicCertificate::read(&source).unwrap();
        let output = temp.path().join("public.crt");
        export(&source, &output, CertificateFormat::Der, &cert).unwrap();
        assert_eq!(fs::read(&output).unwrap(), cert.der);
        export(&source, &output, CertificateFormat::Android, &cert).unwrap();
        assert_eq!(fs::read_to_string(&output).unwrap(), cert.pem);
        let key = source.with_file_name("ca.key");
        let original_key = fs::read(&key).unwrap();
        assert!(export(&source, &key, CertificateFormat::Der, &cert).is_err());
        assert!(export(&source, &source, CertificateFormat::Pem, &cert).is_err());
        let alias = temp.path().join("key-alias");
        fs::hard_link(&key, &alias).unwrap();
        assert!(export(&source, &alias, CertificateFormat::Der, &cert).is_err());
        assert_eq!(fs::read(&key).unwrap(), original_key);
        let invalid_source = temp.path().join("mixed.pem");
        fs::write(
            &invalid_source,
            [cert.pem.as_bytes(), &original_key].concat(),
        )
        .unwrap();
        assert!(PublicCertificate::read(&invalid_source).is_err());

        export(&source, &output, CertificateFormat::Magisk, &cert).unwrap();
        let mut archive = zip::ZipArchive::new(fs::File::open(output).unwrap()).unwrap();
        assert_eq!(archive.len(), 5);
        assert!(
            archive
                .by_name("customize.sh")
                .unwrap()
                .unix_mode()
                .is_some_and(|mode| mode & 0o777 == 0o755)
        );
        let mut pem = String::new();
        archive
            .by_name(&format!(
                "system/etc/security/cacerts/{}",
                android_name(&cert)
            ))
            .unwrap()
            .read_to_string(&mut pem)
            .unwrap();
        assert_eq!(pem, cert.pem);
        engine.stop().await;
    }
}
