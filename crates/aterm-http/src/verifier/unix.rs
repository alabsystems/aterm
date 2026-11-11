// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TLS certificate chain verification against the on-disk system CA store via [`WebPkiServerVerifier`].
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::ServerCertVerifier as _;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};

use super::eku_rejected;

/// Bundle files, in the order OpenSSL looks for them; the first that exists
/// wins.
#[cfg(target_os = "linux")]
const CERTIFICATE_FILES: &[&str] = &[
    "/etc/ssl/certs/ca-certificates.crt",
    "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
    "/etc/pki/tls/certs/ca-bundle.crt",
    "/etc/ssl/ca-bundle.pem",
    "/etc/pki/tls/cacert.pem",
    "/etc/ssl/cert.pem",
    "/opt/etc/ssl/certs/ca-certificates.crt",
    "/etc/ssl/certs/cacert.pem",
];
#[cfg(target_os = "freebsd")]
const CERTIFICATE_FILES: &[&str] = &["/usr/local/etc/ssl/cert.pem"];
#[cfg(target_os = "dragonfly")]
const CERTIFICATE_FILES: &[&str] = &["/usr/local/share/certs/ca-root-nss.crt"];
#[cfg(target_os = "netbsd")]
const CERTIFICATE_FILES: &[&str] = &["/etc/openssl/certs/ca-certificates.crt"];
#[cfg(target_os = "openbsd")]
const CERTIFICATE_FILES: &[&str] = &["/etc/ssl/cert.pem"];
#[cfg(target_os = "solaris")]
const CERTIFICATE_FILES: &[&str] = &["/etc/certs/ca-certificates.crt"];
#[cfg(target_os = "illumos")]
const CERTIFICATE_FILES: &[&str] = &["/etc/ssl/cacert.pem", "/etc/certs/ca-certificates.crt"];
#[cfg(target_os = "haiku")]
const CERTIFICATE_FILES: &[&str] = &["/boot/system/data/ssl/CARootCertificates.pem"];
#[cfg(not(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "solaris",
    target_os = "illumos",
    target_os = "haiku",
)))]
const CERTIFICATE_FILES: &[&str] = &["/etc/ssl/cert.pem"];

/// Hash directories; every one that exists is read, not just the first.
#[cfg(target_os = "linux")]
const CERTIFICATE_DIRS: &[&str] = &[
    "/etc/ssl/certs",
    "/etc/pki/tls/certs",
    "/etc/security/certificates",
];
#[cfg(target_os = "freebsd")]
const CERTIFICATE_DIRS: &[&str] = &["/etc/ssl/certs", "/usr/local/share/certs"];
#[cfg(any(target_os = "illumos", target_os = "solaris"))]
const CERTIFICATE_DIRS: &[&str] = &["/etc/certs/CA"];
#[cfg(target_os = "netbsd")]
const CERTIFICATE_DIRS: &[&str] = &["/etc/openssl/certs"];
#[cfg(target_os = "aix")]
const CERTIFICATE_DIRS: &[&str] = &["/var/ssl/certs"];
#[cfg(not(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "illumos",
    target_os = "solaris",
    target_os = "netbsd",
    target_os = "aix",
)))]
const CERTIFICATE_DIRS: &[&str] = &["/etc/ssl/certs"];

/// Upper bound on any single PEM trust-anchor bundle read from disk.
const MAX_PEM_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// The bundle file to read, if any. `configured` is `$SSL_CERT_FILE`, passed in
/// so the tests need not mutate the process environment.
fn bundle_file(configured: Option<PathBuf>) -> Option<PathBuf> {
    if configured.is_some() {
        // A set variable suppresses the built-in search even when its path is
        // missing.
        return configured.filter(|path| path.is_file());
    }
    CERTIFICATE_FILES
        .iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
}

/// Every hash directory to scan. `configured` is `$SSL_CERT_DIR`, a
/// `:`-separated list that replaces the built-in one; an empty or missing entry
/// contributes nothing.
fn bundle_dirs(configured: Option<PathBuf>) -> Vec<PathBuf> {
    if let Some(configured) = configured {
        return std::env::split_paths(&configured)
            .filter(|path| !path.as_os_str().is_empty() && path.is_dir())
            .collect();
    }
    CERTIFICATE_DIRS
        .iter()
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .collect()
}

/// Decode every certificate in one PEM file, tolerating anything else in it.
fn read_pem_file(path: &Path, out: &mut Vec<Vec<u8>>) {
    read_pem_file_up_to(path, MAX_PEM_FILE_BYTES, out);
}

#[cfg(target_os = "macos")]
const NONBLOCK_OPEN_FLAG: i32 = 0x0004 | 0x0100_0000;
#[cfg(not(target_os = "macos"))]
const NONBLOCK_OPEN_FLAG: i32 = 0o4000 | 0o2_000_000;

fn read_pem_file_up_to(path: &Path, cap: u64, out: &mut Vec<Vec<u8>>) {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    // An unreadable, non-regular, oversized, or non-UTF-8 file contributes
    // nothing; only an empty store overall (`Verifier::from_ders`) is a failure.
    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(NONBLOCK_OPEN_FLAG)
        .open(path)
    else {
        return;
    };
    let Ok(open_meta) = file.metadata() else {
        return;
    };
    if !open_meta.is_file() || open_meta.len() > cap {
        return;
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(open_meta.len())
            .unwrap_or(0)
            .saturating_add(1),
    );
    if file
        .take(cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() as u64 > cap
    {
        return;
    }
    if let Ok(text) = std::str::from_utf8(&bytes) {
        out.extend(crate::pem::decode_certificates_lossy(text));
    }
}

/// Every DER certificate `file` and `dirs` offer, sorted and deduplicated.
fn collect_ders(file: Option<&Path>, dirs: &[PathBuf]) -> Vec<Vec<u8>> {
    let mut ders = Vec::new();
    if let Some(file) = file {
        read_pem_file(file, &mut ders);
    }
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let is_file = entry
                .file_type()
                .map(|ft| ft.is_file() || (ft.is_symlink() && entry.path().is_file()))
                .unwrap_or(false);
            if is_file {
                read_pem_file(&entry.path(), &mut ders);
            }
        }
    }
    ders.sort_unstable();
    ders.dedup();
    ders
}

/// Everything the system trust store offers on this machine.
fn system_root_ders() -> Vec<Vec<u8>> {
    let env_file = std::env::var_os("SSL_CERT_FILE").map(PathBuf::from);
    let env_dir = std::env::var_os("SSL_CERT_DIR")
        .map(PathBuf::from)
        .filter(|dirs| std::env::split_paths(dirs).any(|dir| !dir.as_os_str().is_empty()));
    let (file, dirs) = if env_file.is_some() || env_dir.is_some() {
        (
            env_file.and_then(|file| bundle_file(Some(file))),
            env_dir.map_or_else(Vec::new, |dirs| bundle_dirs(Some(dirs))),
        )
    } else {
        (bundle_file(None), bundle_dirs(None))
    };
    collect_ders(file.as_deref(), &dirs)
}

/// Certificate chain verifier backed by the on-disk system CA store.
#[derive(Debug)]
pub(super) struct Verifier {
    inner: Arc<WebPkiServerVerifier>,
}

impl Verifier {
    /// Builds a verifier from the system CA store and `extra_roots`.
    pub(super) fn new(
        extra_roots: Vec<CertificateDer<'static>>,
        provider: Arc<CryptoProvider>,
    ) -> Result<Self, rustls::Error> {
        Self::from_ders(extra_roots, system_root_ders(), provider)
    }

    /// The half of [`Self::new`] that does not touch the filesystem, so the
    /// empty-store branch is reachable from a test.
    fn from_ders(
        extra_roots: Vec<CertificateDer<'static>>,
        system_roots: Vec<Vec<u8>>,
        provider: Arc<CryptoProvider>,
    ) -> Result<Self, rustls::Error> {
        let mut store = rustls::RootCertStore::empty();
        for root in extra_roots {
            store.add(root)?;
        }
        let (_added, _ignored) =
            store.add_parsable_certificates(system_roots.into_iter().map(CertificateDer::from));
        if store.is_empty() {
            return Err(rustls::Error::General(
                "no CA certificates could be loaded from the system trust store".to_owned(),
            ));
        }
        let inner = WebPkiServerVerifier::builder_with_provider(store.into(), provider)
            .build()
            .map_err(|error| {
                rustls::Error::General(format!("system trust store is unusable: {error}"))
            })?;
        Ok(Self { inner })
    }

    /// Verifies the presented server certificate chain against the system roots.
    pub(super) fn verify(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        _server_text: &str,
        ocsp_response: Option<&[u8]>,
        now: UnixTime,
    ) -> Result<(), rustls::Error> {
        self.inner
            .verify_server_cert(
                end_entity,
                intermediates,
                server_name,
                ocsp_response.unwrap_or(&[]),
                now,
            )
            .map(|_verified| ())
            .map_err(|error| match &error {
                rustls::Error::InvalidCertificate(rustls::CertificateError::InvalidPurpose)
                | rustls::Error::InvalidCertificate(
                    rustls::CertificateError::InvalidPurposeContext { .. },
                ) => eku_rejected(),
                _ => error,
            })
    }
}

#[cfg(test)]
#[path = "unix_tests.rs"]
mod tests;
