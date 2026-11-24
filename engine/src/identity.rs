pub const NAME: &str = "Recho";
pub const SLUG: &str = "recho";
#[cfg(target_os = "macos")]
pub const DATA_DIRECTORY: &str = "Library/Application Support/Recho";
#[cfg(windows)]
pub const DATA_DIRECTORY: &str = "Recho";
#[cfg(all(not(target_os = "macos"), not(windows)))]
pub const DATA_DIRECTORY: &str = ".local/share/Recho";
pub const CA_HOST: &str = "cert.recho.local";
pub const CA_PATH: &str = "/ca";
pub const CERTIFICATE_DOWNLOAD_URL: &str = "http://cert.recho.local/ca";
pub const CA_NAME: &str = "Recho Local CA";
pub const CA_FILENAME: &str = "recho-ca.crt";
