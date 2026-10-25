//! The relay's certificate, for `wss://` and QUIC.
//!
//! With certificate files configured, they are read at start and read again
//! whenever they change (a renewal), without dropping a connection; a
//! renewal that fails to load keeps the old certificate. Without them, the
//! relay serves the weekly certificate of GROUP.md §3.7, the next one each
//! week: raw QUIC members do not check it (members never rely on the relay
//! for confidentiality), and browsers pin its hash, which they compute.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use peerconnect::group::certificate;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivateSec1KeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;

/// How often certificate files are checked for changes.
const RELOAD_EVERY: Duration = Duration::from_secs(60);

/// Certificate files, in PEM: the chain (leaf first) and its private key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tls {
    pub cert: PathBuf,
    pub key: PathBuf,
}

/// The certificate currently served.
#[derive(Debug)]
pub(crate) struct Certificate {
    /// The certificate, and the week it is for (without files).
    current: RwLock<(u64, Arc<CertifiedKey>)>,
    files: Option<Tls>,
}

impl Certificate {
    /// Loads `files`, or serves the weekly certificate without them.
    pub(crate) fn new(files: Option<&Tls>) -> Result<Arc<Certificate>, String> {
        let current = match files {
            Some(files) => (0, Arc::new(load(files)?)),
            None => {
                let week = certificate::week_of(unix_now());
                (week, Arc::new(weekly(week)))
            }
        };
        Ok(Arc::new(Certificate { current: RwLock::new(current), files: files.cloned() }))
    }

    /// Whether it comes from files, and so can be trusted by browsers.
    pub(crate) fn is_configured(&self) -> bool {
        self.files.is_some()
    }

    /// A server configuration that serves this certificate for `alpn`, over
    /// TLS 1.3 only for QUIC.
    pub(crate) fn server_config(self: &Arc<Self>, alpn: &[&[u8]], quic: bool) -> rustls::ServerConfig {
        let versions = if quic { &[&rustls::version::TLS13][..] } else { rustls::DEFAULT_VERSIONS };
        let mut config = rustls::ServerConfig::builder_with_provider(provider())
            .with_protocol_versions(versions)
            .expect("the provider supports the default versions")
            .with_no_client_auth()
            .with_cert_resolver(self.clone());
        config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
        config
    }

    /// Re-reads the files whenever they change, until the relay stops.
    pub(crate) async fn watch(self: Arc<Self>) {
        let Some(files) = self.files.clone() else { return };
        let mut seen = modified(&files);
        loop {
            tokio::time::sleep(RELOAD_EVERY).await;
            match self.reload(&mut seen) {
                Ok(true) => eprintln!("reloaded the certificate from {}", files.cert.display()),
                Ok(false) => {}
                Err(err) => eprintln!("keeping the old certificate: {err}"),
            }
        }
    }

    /// Loads the files again if they changed since `seen`; whether it did.
    /// Files that do not load (caught halfway through a renewal, say) are
    /// tried again next time.
    fn reload(&self, seen: &mut Option<(SystemTime, SystemTime)>) -> Result<bool, String> {
        let Some(files) = &self.files else { return Ok(false) };
        let now = modified(files);
        if now == *seen {
            return Ok(false);
        }
        self.current.write().unwrap().1 = Arc::new(load(files)?);
        *seen = now;
        Ok(true)
    }

    /// The weekly certificate for `unix_seconds`, made when its week begins.
    fn weekly_at(&self, unix_seconds: u64) -> Arc<CertifiedKey> {
        let week = certificate::week_of(unix_seconds);
        {
            let current = self.current.read().unwrap();
            if current.0 == week {
                return current.1.clone();
            }
        }
        let key = Arc::new(weekly(week));
        *self.current.write().unwrap() = (week, key.clone());
        key
    }

    #[cfg(test)]
    pub(crate) fn current(&self) -> Arc<CertifiedKey> {
        self.current.read().unwrap().1.clone()
    }
}

impl ResolvesServerCert for Certificate {
    fn resolve(&self, _: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        if self.files.is_none() {
            return Some(self.weekly_at(unix_now()));
        }
        Some(self.current.read().unwrap().1.clone())
    }
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

pub(crate) fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn load(files: &Tls) -> Result<CertifiedKey, String> {
    let chain = CertificateDer::pem_file_iter(&files.cert)
        .and_then(|certs| certs.collect::<Result<Vec<_>, _>>())
        .map_err(|err| format!("cannot read certificates from {}: {err}", files.cert.display()))?;
    if chain.is_empty() {
        return Err(format!("no certificate in {}", files.cert.display()));
    }
    let key = PrivateKeyDer::from_pem_file(&files.key)
        .map_err(|err| format!("cannot read a private key from {}: {err}", files.key.display()))?;
    CertifiedKey::from_der(chain, key, &provider())
        .map_err(|err| format!("{} and {} do not make a certificate: {err}", files.cert.display(), files.key.display()))
}

fn weekly(week: u64) -> CertifiedKey {
    let made = certificate::certificate(week);
    let key = PrivateKeyDer::Sec1(PrivateSec1KeyDer::from(made.key));
    CertifiedKey::from_der(vec![CertificateDer::from(made.der)], key, &provider()).expect("a usable weekly certificate")
}

fn modified(files: &Tls) -> Option<(SystemTime, SystemTime)> {
    let at = |path: &PathBuf| std::fs::metadata(path).and_then(|m| m.modified()).ok();
    Some((at(&files.cert)?, at(&files.key)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_pair(dir: &std::path::Path, name: &str) -> Tls {
        let made = rcgen::generate_simple_self_signed(vec![name.to_owned()]).unwrap();
        let tls = Tls { cert: dir.join(format!("{name}.crt")), key: dir.join(format!("{name}.key")) };
        std::fs::write(&tls.cert, made.cert.pem()).unwrap();
        std::fs::write(&tls.key, made.signing_key.serialize_pem()).unwrap();
        tls
    }

    #[test]
    fn loads_files_and_reports_bad_ones() {
        let dir = std::env::temp_dir().join(format!("peerconnect-tls-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = write_pair(&dir, "a.example");
        let b = write_pair(&dir, "b.example");
        assert!(Certificate::new(Some(&a)).unwrap().is_configured());
        let mixed = Tls { cert: a.cert.clone(), key: b.key.clone() };
        assert!(Certificate::new(Some(&mixed)).unwrap_err().contains("do not make a certificate"));
        let missing = Tls { cert: dir.join("nope.crt"), key: a.key.clone() };
        assert!(Certificate::new(Some(&missing)).unwrap_err().contains("cannot read certificates"));
        // A renewal is picked up; a broken one keeps what was there.
        let renewed = Certificate::new(Some(&a)).unwrap();
        let mut seen = modified(&a);
        assert_eq!(renewed.reload(&mut seen), Ok(false));
        let before = renewed.current().cert[0].clone();
        std::thread::sleep(Duration::from_millis(20));
        let c = write_pair(&dir, "c.example");
        std::fs::rename(&c.cert, &a.cert).unwrap();
        std::fs::rename(&c.key, &a.key).unwrap();
        assert_eq!(renewed.reload(&mut seen), Ok(true));
        assert_ne!(renewed.current().cert[0], before);
        let after = renewed.current().cert[0].clone();
        std::thread::sleep(Duration::from_millis(20));
        // Halfway through a renewal: the new certificate, the old key.
        let d = write_pair(&dir, "d.example");
        std::fs::rename(&d.cert, &a.cert).unwrap();
        assert!(renewed.reload(&mut seen).is_err());
        assert_eq!(renewed.current().cert[0], after);
        assert!(renewed.reload(&mut seen).is_err(), "tried again");
        std::fs::rename(&d.key, &a.key).unwrap();
        assert_eq!(renewed.reload(&mut seen), Ok(true), "and loaded once whole");
        assert_ne!(renewed.current().cert[0], after);

        let weekly = Certificate::new(None).unwrap();
        assert!(!weekly.is_configured());
        let now = unix_now();
        let this_week = certificate::certificate(certificate::week_of(now));
        assert_eq!(weekly.weekly_at(now).cert[0].as_ref(), this_week.der, "this week's");
        let next = weekly.weekly_at(now + certificate::WEEK);
        assert_eq!(next.cert[0].as_ref(), certificate::certificate(certificate::week_of(now) + 1).der, "then the next");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
