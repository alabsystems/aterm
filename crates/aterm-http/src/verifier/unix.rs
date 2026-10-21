// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Linux and the BSDs: the distribution's on-disk CA store, verified by webpki.
//!
//! There is no OS trust-evaluation call here: the system store is a set of
//! files. This arm finds them the way OpenSSL does and hands them to `rustls`'s
//! own [`WebPkiServerVerifier`].
//!
//! Apple test builds compile it too, and its tests run against macOS's
//! `/etc/ssl/cert.pem`, the same shape of OpenSSL bundle. A distro's exact paths
//! and an `openssl rehash` symlink farm still need a Linux host.
//!
//! Roots are found as `rustls-native-certs` 0.8.4 finds them:
//!
//! * Neither `$SSL_CERT_FILE` nor `$SSL_CERT_DIR` set: the first existing path
//!   of [`CERTIFICATE_FILES`], plus every existing path of [`CERTIFICATE_DIRS`].
//! * Either set: only those. The environment replaces the built-in search, never
//!   adds to it, so an operator pinning one CA gets exactly that CA and a stale
//!   variable fails rather than restoring the system store. `$SSL_CERT_DIR` is a
//!   `:`-separated list. A configured path that does not exist contributes
//!   nothing; an empty store is an error at [`Verifier::new`].
//! * Directories are read non-recursively, following symlinks; a dangling one
//!   is skipped.
//!
//! A real `/etc/ssl/certs` holds `TRUSTED CERTIFICATE` blocks and certificates
//! webpki refuses, so parsing is [`crate::pem::decode_certificates_lossy`] and
//! roots go in through `RootCertStore::add_parsable_certificates`. Tolerance
//! stops there: an empty store is an error, never a verifier.
//!
//! THE ENVIRONMENT IS EXCLUSIVE, NOT ADDITIVE.
//! `rustls_native_certs::load_native_certs` (lib.rs:118-125) reads:
//!
//! ```text
//! let paths = CertPaths::from_env();
//! match (&paths.dirs, &paths.file) {
//!     (v, _) if !v.is_empty() => paths.load(),   // $SSL_CERT_DIR  set -> env ONLY
//!     (_, Some(_))            => paths.load(),   // $SSL_CERT_FILE set -> env ONLY
//!     _ => platform::load_native_certs(),        // neither -> the built-in search
//! }
//! ```
//!
//! So if EITHER variable is set, the built-in lists are never consulted. Getting
//! this backwards is not a stylistic difference — it defeats the configuration:
//! an operator who points `$SSL_CERT_FILE` at a single pinned CA would silently
//! get that CA *plus* the entire system store, and a stale variable pointing at
//! a deleted file would silently restore the full store instead of failing.
//!
//! * **Neither variable set** — the bundle file is the FIRST existing path of
//!   [`CERTIFICATE_FILES`], plus every existing path of [`CERTIFICATE_DIRS`].
//! * **Either variable set** — ONLY `$SSL_CERT_FILE` and/or `$SSL_CERT_DIR`.
//!   `$SSL_CERT_DIR` is a `:`-separated LIST of directories, as OpenSSL and the
//!   incumbent read it. A configured path that does not exist contributes
//!   nothing, which can leave an empty store; that is a hard error at
//!   [`Verifier::new`], and failing closed is the intended outcome.
//! * Directories are read non-recursively. Symlinks are resolved through
//!   `fs::metadata` (this is what `openssl rehash` creates) and a dangling one
//!   is skipped in silence.
//!
//! # Two tolerances that are deliberate, not sloppy
//!
//! A real `/etc/ssl/certs` is not a curated bundle. It contains `TRUSTED
//! CERTIFICATE` blocks (OpenSSL's aux-info form, which is NOT a bare
//! `Certificate` and must not be decoded as one), occasionally other labels, and
//! certificates that `webpki` legitimately refuses. So:
//!
//! * parsing uses [`crate::pem::decode_certificates_lossy`], which skips a block
//!   it does not understand instead of failing the file;
//! * roots go in through `RootCertStore::add_parsable_certificates`, which drops
//!   the ones webpki rejects rather than refusing the whole store.
//!
//! Tolerance stops there. **If the store ends up empty this arm returns an
//! error rather than a verifier**, so a machine with no CA store fails every
//! HTTPS connection loudly at configuration time instead of quietly trusting
//! nothing — or, far worse, quietly trusting anything.
//!
//! # Revocation: none
//!
//! This arm performs NO revocation checking, like `rustls-platform-verifier`'s
//! Unix arm: `WebPkiServerVerifier` ignores a stapled OCSP response unless it
//! was built with CRLs, and it is not. macOS and Windows DO check end-entity
//! revocation.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::ServerCertVerifier as _;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};

use super::eku_rejected;

/// Bundle files, in the order OpenSSL looks for them; the first that exists
/// wins.
///
/// Per target, not a union: a cross-OS union would widen trust in the default
/// configuration. These are `openssl-probe` 0.2.1's own `#[cfg]` arms.
#[cfg(target_os = "linux")]
const CERTIFICATE_FILES: &[&str] = &[
    "/etc/ssl/certs/ca-certificates.crt", // Debian, Ubuntu, Gentoo
    "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem", // CentOS, RHEL 7
    "/etc/pki/tls/certs/ca-bundle.crt",   // Fedora, RHEL 6
    "/etc/ssl/ca-bundle.pem",             // openSUSE
    "/etc/pki/tls/cacert.pem",            // OpenELEC
    "/etc/ssl/cert.pem",                  // Alpine
    "/opt/etc/ssl/certs/ca-certificates.crt", // Entware
    "/etc/ssl/certs/cacert.pem",          // OpenHarmony
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
/// Per-target for the same reason as [`CERTIFICATE_FILES`].
#[cfg(target_os = "linux")]
const CERTIFICATE_DIRS: &[&str] = &[
    "/etc/ssl/certs",             // SLES
    "/etc/pki/tls/certs",         // Fedora, RHEL
    "/etc/security/certificates", // OpenHarmony
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

/// The bundle file to read, if any. `configured` is `$SSL_CERT_FILE`, passed in
/// so the tests need not mutate the process environment.
fn bundle_file(configured: Option<PathBuf>) -> Option<PathBuf> {
    if configured.is_some() {
        // A set variable suppresses the built-in search even when its path is
        // missing.
        return configured.filter(|path| path.exists());
    }
    CERTIFICATE_FILES
        .iter()
        .map(PathBuf::from)
        .find(|path| path.exists())
}

/// Every hash directory to scan. `configured` is `$SSL_CERT_DIR`, a
/// `:`-separated list that replaces the built-in one; an empty or missing entry
/// contributes nothing.
fn bundle_dirs(configured: Option<PathBuf>) -> Vec<PathBuf> {
    if let Some(configured) = configured {
        return std::env::split_paths(&configured)
            .filter(|path| !path.as_os_str().is_empty() && path.exists())
            .collect();
    }
    CERTIFICATE_DIRS
        .iter()
        .map(PathBuf::from)
        .filter(|path| path.exists())
        .collect()
}

/// Decode every certificate in one PEM file, tolerating anything else in it.
fn read_pem_file(path: &Path, out: &mut Vec<Vec<u8>>) {
    // An unreadable or non-UTF-8 file contributes nothing; only an empty store
    // overall (`Verifier::from_ders`) is a failure.
    if let Ok(text) = std::fs::read_to_string(path) {
        out.extend(crate::pem::decode_certificates_lossy(&text));
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
            let path = entry.path();
            // `fs::metadata` follows symlinks (an `openssl rehash` directory is
            // mostly symlinks); a dangling one is skipped.
            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };
            if metadata.is_file() {
                read_pem_file(&path, &mut ders);
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
    // A `$SSL_CERT_DIR` that lists nothing (`""`, `":"`) is not configured.
    let env_dir = std::env::var_os("SSL_CERT_DIR")
        .map(PathBuf::from)
        .filter(|dirs| std::env::split_paths(dirs).any(|dir| !dir.as_os_str().is_empty()));
    // Either variable suppresses both built-in lists; otherwise `$SSL_CERT_FILE`
    // alone would still pull in every built-in directory.
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

/// Chain verification against the on-disk system store.
#[derive(Debug)]
pub(super) struct Verifier {
    inner: Arc<WebPkiServerVerifier>,
}

impl Verifier {
    /// `extra_roots` is empty on every shipped path. The store is read once,
    /// here, not per handshake.
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
        // Extra anchors are parsed strictly: a bad one is reported, not skipped.
        for root in extra_roots {
            store.add(root)?;
        }
        let (_added, _ignored) =
            store.add_parsable_certificates(system_roots.into_iter().map(CertificateDer::from));
        // Fail closed: an empty store would reject every chain like a network
        // fault, so it is reported as a configuration failure.
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

    /// The presented chain, judged by webpki against the system roots.
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
            // webpki reports a wrong or absent EKU as `InvalidPurpose`; map it to
            // the outcome the Apple and Windows arms report.
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
mod tests {
    use super::*;

    fn provider() -> Arc<CryptoProvider> {
        crate::tls::init_crypto();
        rustls::crypto::CryptoProvider::get_default()
            .cloned()
            .unwrap_or_else(|| Arc::new(crate::tls::provider()))
    }

    #[test]
    fn an_empty_system_store_is_a_loud_failure_not_a_silent_verifier() {
        let error = Verifier::from_ders(Vec::new(), Vec::new(), provider())
            .expect_err("an empty store must not produce a verifier");
        assert!(
            error.to_string().contains("system trust store"),
            "unhelpful error: {error}"
        );
    }

    #[test]
    fn unparseable_system_roots_are_skipped_but_do_not_take_the_store_with_them() {
        // A real distro store contains certificates webpki refuses.
        let good = include_bytes!("../testdata/tls/root.der").to_vec();
        let junk = vec![0u8; 8];
        Verifier::from_ders(Vec::new(), vec![junk, good], provider())
            .expect("one usable root among the rubbish is still a usable store");
    }

    #[test]
    fn an_unusable_extra_anchor_is_an_error_rather_than_a_skip() {
        // Extra anchors are configuration, not discovery.
        let good = include_bytes!("../testdata/tls/root.der").to_vec();
        assert!(
            Verifier::from_ders(
                vec![CertificateDer::from(vec![0u8; 8])],
                vec![good],
                provider()
            )
            .is_err()
        );
    }

    #[test]
    fn the_built_in_search_finds_this_machines_real_bundle() {
        // Discovery, the tolerant PEM path, and `add_parsable_certificates`,
        // over whatever multi-hundred-certificate OpenSSL bundle this host
        // actually has. On macOS that is `/etc/ssl/cert.pem`; on Linux it is
        // whichever of `CERTIFICATE_FILES` the distro ships. A host with
        // neither has nothing to assert: COULD NOT RUN, never a pass.
        let file = bundle_file(None);
        let dirs = bundle_dirs(None);
        if file.is_none() && dirs.is_empty() {
            aterm_spec::host::could_not_run("this host has no OpenSSL-shaped CA store");
        }
        let ders = collect_ders(file.as_deref(), &dirs);
        assert!(
            ders.len() > 20,
            "found only {} certificates in {file:?} + {dirs:?}; a real CA store has hundreds",
            ders.len()
        );
        // Usable as a root store: the lossy parser produced certificates.
        let verifier = Verifier::from_ders(Vec::new(), ders, provider())
            .expect("the host's own CA store must build a verifier");
        // The store validates the captured github.com chain the Apple arm is
        // checked against, at the same pinned instant.
        let leaf = CertificateDer::from(include_bytes!("../testdata/tls/gh-leaf.der").as_slice());
        let int0 = CertificateDer::from(include_bytes!("../testdata/tls/gh-int0.der").as_slice());
        let int1 = CertificateDer::from(include_bytes!("../testdata/tls/gh-int1.der").as_slice());
        let name = ServerName::try_from("github.com").expect("a usable server name");
        let at = UnixTime::since_unix_epoch(std::time::Duration::from_secs(1_787_986_773));
        let accepted = verifier
            .verify(
                &leaf,
                &[int0.clone(), int1.clone()],
                &name,
                "github.com",
                None,
                at,
            )
            .is_ok();
        if !accepted {
            aterm_spec::host::could_not_run(
                "this host's OpenSSL bundle does not carry the anchor for the captured \
                 github.com chain",
            );
        }
        // The arming twin: the same store must refuse the same chain for a name
        // it does not carry, so the acceptance above is not "accepts everything".
        let wrong = ServerName::try_from("example.invalid").expect("a usable server name");
        assert!(
            verifier
                .verify(&leaf, &[int0, int1], &wrong, "example.invalid", None, at)
                .is_err(),
            "the system store accepted a real chain for a name it does not carry"
        );
    }

    #[test]
    fn the_shipped_constructor_reads_this_hosts_store() {
        // The shipped entry point, including `system_root_ders`'s environment
        // lookups.
        match Verifier::new(Vec::new(), provider()) {
            Ok(_) => {}
            Err(error) => {
                // Only on a host with no CA store, and then as the loud failure.
                assert!(
                    error.to_string().contains("system trust store"),
                    "unexpected failure: {error}"
                );
                aterm_spec::host::could_not_run("this host has no OpenSSL-shaped CA store");
            }
        }
    }

    /// The environment replaces the built-in search; it does not add to it.
    #[test]
    fn a_configured_path_replaces_the_built_in_search() {
        // A configured path that does not exist yields nothing, not the system
        // store: a stale variable breaks TLS loudly.
        let missing = PathBuf::from("/nonexistent/aterm/definitely/not/here.pem");
        assert_eq!(bundle_file(Some(missing.clone())), None);
        assert!(bundle_dirs(Some(missing)).is_empty());
        // An existing path wins outright, even if it is not a built-in candidate.
        let real = PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/testdata/tls/root.der"
        ));
        assert!(
            real.exists(),
            "the fixture must be where the test says it is"
        );
        assert_eq!(bundle_file(Some(real.clone())), Some(real));
    }

    /// `$SSL_CERT_DIR` is a `:`-separated list, as OpenSSL and
    /// `rustls-native-certs` read it.
    #[test]
    fn a_configured_cert_dir_is_a_list_of_directories() {
        let a = concat!(env!("CARGO_MANIFEST_DIR"), "/src/testdata");
        let b = concat!(env!("CARGO_MANIFEST_DIR"), "/src/testdata/tls");
        let both = vec![PathBuf::from(a), PathBuf::from(b)];
        assert_eq!(bundle_dirs(Some(PathBuf::from(format!("{a}:{b}")))), both);
        // An entry that does not exist contributes nothing; the others still count.
        assert_eq!(
            bundle_dirs(Some(PathBuf::from(format!(
                "{a}:/nonexistent/aterm/certs::{b}"
            )))),
            both
        );
    }
}
