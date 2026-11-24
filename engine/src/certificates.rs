use crate::{
    storage::{atomic_write, private_dir, private_file},
    wire::{Error, Result},
};
use chrono::{Datelike, TimeDelta, Utc};
use rcgen::{
    BasicConstraints, Certificate, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, KeyUsagePurpose, SerialNumber,
};
use rustls::{ClientConfig, RootCertStore, ServerConfig, pki_types::PrivatePkcs8KeyDer};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::Mutex;
use uuid::Uuid;

fn parameters(names: Vec<String>, days: i64) -> Result<CertificateParams> {
    let mut params = CertificateParams::new(names).map_err(Error::bad)?;
    let start = Utc::now().date_naive() - TimeDelta::days(1);
    let end = Utc::now().date_naive() + TimeDelta::days(days);
    params.not_before = rcgen::date_time_ymd(start.year(), start.month() as u8, start.day() as u8);
    params.not_after = rcgen::date_time_ymd(end.year(), end.month() as u8, end.day() as u8);
    params.serial_number = Some(SerialNumber::from_slice(Uuid::new_v4().as_bytes()));
    Ok(params)
}

pub struct Authority {
    pub ca_path: PathBuf,
    issuer: Certificate,
    key: KeyPair,
    cache: Mutex<BTreeMap<String, Arc<ServerConfig>>>,
}

pub struct PublicCertificate {
    pub pem: String,
    pub der: Vec<u8>,
    pub subject_der: Vec<u8>,
    pub subject: String,
    pub issuer: String,
    pub serial: String,
    pub not_before: String,
    pub not_after: String,
}

impl PublicCertificate {
    pub fn read(path: &Path) -> Result<Self> {
        const MAX_PEM_SIZE: usize = 64 * 1024;
        let mut pem = String::new();
        fs::File::open(path)?
            .take((MAX_PEM_SIZE + 1) as u64)
            .read_to_string(&mut pem)?;
        let text = pem.trim();
        if pem.len() > MAX_PEM_SIZE
            || !text.starts_with("-----BEGIN CERTIFICATE-----")
            || !text.ends_with("-----END CERTIFICATE-----")
            || text.matches("-----BEGIN ").count() != 1
            || text.matches("-----END ").count() != 1
        {
            return Err(Error::bad("Expected exactly one public CA certificate"));
        }
        let der = rustls_pemfile::certs(&mut pem.as_bytes())
            .next()
            .ok_or_else(|| Error::bad("CA certificate missing"))??
            .to_vec();
        let (trailing, cert) = x509_parser::parse_x509_certificate(&der).map_err(Error::bad)?;
        if !trailing.is_empty() || !cert.is_ca() {
            return Err(Error::bad("Expected exactly one public CA certificate"));
        }
        Ok(Self {
            pem,
            subject_der: cert.subject().as_raw().to_vec(),
            subject: cert.subject().to_string(),
            issuer: cert.issuer().to_string(),
            serial: cert.raw_serial_as_string(),
            not_before: cert
                .validity()
                .not_before
                .to_rfc2822()
                .map_err(Error::bad)?,
            not_after: cert.validity().not_after.to_rfc2822().map_err(Error::bad)?,
            der,
        })
    }
}

impl Authority {
    pub fn info(&self) -> Result<serde_json::Value> {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        let certificate = PublicCertificate::read(&self.ca_path)?;
        Ok(serde_json::json!({
            "pem": certificate.pem,
            "der": STANDARD.encode(certificate.der),
            "subjectDER": STANDARD.encode(certificate.subject_der),
            "subject": certificate.subject,
            "issuer": certificate.issuer,
            "serial": certificate.serial,
            "notBefore": certificate.not_before,
            "notAfter": certificate.not_after,
        }))
    }
    pub async fn open(data_dir: &Path) -> Result<Self> {
        let directory = data_dir.join("certificates");
        private_dir(&directory)?;
        let ca_path = directory.join("ca.pem");
        let key_path = directory.join("ca.key");
        let cert_exists = ca_path.try_exists()?;
        let key_exists = key_path.try_exists()?;
        if cert_exists != key_exists {
            return Err(Error::new(
                500,
                "Local CA is incomplete. Restore both ca.pem and ca.key, or remove the certificates directory to create a new CA.",
            ));
        }
        if !cert_exists {
            let key = KeyPair::generate().map_err(Error::bad)?;
            let mut params = parameters(vec![], 3650)?;
            params
                .distinguished_name
                .push(DnType::CommonName, crate::identity::CA_NAME);
            params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
            params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
            let ca = params.self_signed(&key).map_err(Error::bad)?;
            atomic_write(&key_path, key.serialize_pem().as_bytes())?;
            atomic_write(&ca_path, ca.pem().as_bytes())?;
        }
        private_file(&key_path)?;
        let ca_pem = fs::read_to_string(&ca_path)?;
        let key = KeyPair::from_pem(&fs::read_to_string(&key_path)?).map_err(Error::bad)?;
        let ca_der = rustls_pemfile::certs(&mut ca_pem.as_bytes())
            .next()
            .ok_or_else(|| Error::bad("Local CA certificate is missing"))??;
        let (_, parsed) = x509_parser::parse_x509_certificate(&ca_der).map_err(Error::bad)?;
        if parsed.public_key().raw != key.public_key_der() {
            return Err(Error::new(
                500,
                "The local CA certificate and private key do not match.",
            ));
        }
        let issuer = CertificateParams::from_ca_cert_pem(&ca_pem)
            .map_err(Error::bad)?
            .self_signed(&key)
            .map_err(Error::bad)?;
        Ok(Self {
            ca_path,
            issuer,
            key,
            cache: Mutex::new(BTreeMap::new()),
        })
    }
    pub async fn certificate(&self, host: &str) -> Result<Arc<ServerConfig>> {
        let host = host.trim_matches(['[', ']']);
        let is_ip = host.parse::<IpAddr>().is_ok();
        let host = if is_ip {
            host.to_string()
        } else {
            url::Host::parse(host)
                .map_err(Error::bad)?
                .to_string()
                .to_ascii_lowercase()
        };
        if !is_ip
            && (host.len() > 253
                || host.split('.').any(|label| {
                    label.is_empty()
                        || label.len() > 63
                        || label.starts_with('-')
                        || label.ends_with('-')
                        || !label
                            .bytes()
                            .all(|v| v.is_ascii_alphanumeric() || v == b'-')
                }))
        {
            return Err(Error::bad("Invalid TLS hostname"));
        }
        let mut cache = self.cache.lock().await;
        if let Some(config) = cache.get(&host) {
            return Ok(config.clone());
        }
        let key = KeyPair::generate().map_err(Error::bad)?;
        let mut params = parameters(vec![host.clone()], 30)?;
        params.distinguished_name.push(DnType::CommonName, &host);
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let cert = params
            .signed_by(&key, &self.issuer, &self.key)
            .map_err(Error::bad)?;
        let mut config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().clone()],
                PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
            )
            .map_err(Error::upstream)?;
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        let config = Arc::new(config);
        if cache.len() >= 100 {
            cache.pop_first();
        }
        cache.insert(host, config.clone());
        Ok(config)
    }
}

pub fn client_config(extra_ca: Option<&Path>) -> Result<Arc<ClientConfig>> {
    let mut roots = RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs().certs {
        let _ = roots.add(cert);
    }
    #[cfg(target_os = "android")]
    for directory in [
        "/apex/com.android.conscrypt/cacerts",
        "/system/etc/security/cacerts",
    ] {
        if let Ok(entries) = fs::read_dir(directory) {
            for entry in entries.flatten().take(4096) {
                if entry
                    .metadata()
                    .is_ok_and(|m| m.is_file() && m.len() <= 64 * 1024)
                {
                    if let Ok(bytes) = fs::read(entry.path()) {
                        for cert in rustls_pemfile::certs(&mut bytes.as_slice()).flatten() {
                            let _ = roots.add(cert);
                        }
                    }
                }
            }
        }
    }
    if let Some(path) = extra_ca {
        for cert in rustls_pemfile::certs(&mut fs::read(path)?.as_slice()) {
            roots.add(cert?).map_err(Error::bad)?;
        }
    }
    if roots.is_empty() {
        return Err(Error::new(500, "No trusted TLS roots could be loaded"));
    }
    let mut config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn persisted_ca_is_reused_and_mismatched_or_partial_pairs_are_preserved() {
        rustls::crypto::ring::default_provider()
            .install_default()
            .ok();
        let directory = tempfile::tempdir().unwrap();
        let ca = Authority::open(directory.path()).await.unwrap();
        let pem = fs::read(&ca.ca_path).unwrap();
        let key_path = directory.path().join("certificates/ca.key");
        let key = fs::read(&key_path).unwrap();
        drop(ca);
        let reopened = Authority::open(directory.path()).await.unwrap();
        assert_eq!(fs::read(&reopened.ca_path).unwrap(), pem);
        assert_eq!(fs::read(&key_path).unwrap(), key);
        let other = KeyPair::generate().unwrap().serialize_pem();
        atomic_write(&key_path, other.as_bytes()).unwrap();
        let failure = Authority::open(directory.path()).await.err().unwrap();
        assert!(failure.message.contains("do not match"));
        assert_eq!(fs::read(&key_path).unwrap(), other.as_bytes());
        fs::remove_file(&key_path).unwrap();
        assert!(
            Authority::open(directory.path())
                .await
                .err()
                .unwrap()
                .message
                .contains("incomplete")
        );
        assert_eq!(fs::read(&reopened.ca_path).unwrap(), pem);
    }

    #[tokio::test]
    async fn generated_leaf_has_a_trusted_hostname_and_valid_signature() {
        rustls::crypto::ring::default_provider()
            .install_default()
            .ok();
        let directory = tempfile::tempdir().unwrap();
        let authority = Authority::open(directory.path()).await.unwrap();
        assert!(authority.certificate("bad\nname.test").await.is_err());
        let server = authority.certificate("local.test").await.unwrap();
        assert!(Arc::ptr_eq(
            &server,
            &authority.certificate("local.test").await.unwrap()
        ));
        let client = client_config(Some(&authority.ca_path)).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let accept = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            tokio_rustls::TlsAcceptor::from(server)
                .accept(socket)
                .await
                .unwrap()
        });
        let socket = tokio::net::TcpStream::connect(address).await.unwrap();
        tokio_rustls::TlsConnector::from(client)
            .connect("local.test".try_into().unwrap(), socket)
            .await
            .unwrap();
        accept.await.unwrap();
    }
}
