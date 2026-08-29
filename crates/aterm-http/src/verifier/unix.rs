// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Linux and the BSDs: the distribution's on-disk CA store, verified by webpki.
//!
//! There is no OS trust-evaluation call on these platforms — the "system trust
//! store" IS a set of files. So this arm does exactly two things: find those
//! files the way OpenSSL does, and hand what they contain to `rustls`'s own
//! [`WebPkiServerVerifier`], which is already in the graph (`rustls-webpki`,
//! via `rustls`) and needs no new package.
//!
//! # This file is COMPILED, and TESTED, on macOS too
//!
//! It is not the selected arm there — [`super::apple`] is — but it is compiled
//! on every Unix so that a Mac-only `cargo build` still type-checks it, and
//! [`tests`] drives it natively: macOS ships `/etc/ssl/cert.pem`, which is the
//! same shape of multi-hundred-certificate OpenSSL bundle a Linux box has, so
//! discovery, the tolerant PEM path and the chain math all execute for real.
//! What that still does NOT cover is the exact set of paths a given distro puts
//! its store at, and the behaviour of an `openssl rehash` symlink farm. Those
//! need a Linux host.
//!
//! # Where the roots come from
//!
//! Reproducing `rustls-native-certs` 0.8.4, which is what
//! `rustls-platform-verifier` calls underneath.
//!
//! THE ENVIRONMENT IS EXCLUSIVE, NOT ADDITIVE, and an earlier version of this
//! header claimed the opposite. `rustls_native_certs::load_native_certs`
//! (lib.rs:118-125) reads:
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
//! Measured on this host before the fix: 129 roots where the incumbent loads 1.
//!
//! * **Neither variable set** — the bundle file is the FIRST existing path of
//!   [`CERTIFICATE_FILES`], plus every existing path of [`CERTIFICATE_DIRS`].
//! * **Either variable set** — ONLY `$SSL_CERT_FILE` and/or `$SSL_CERT_DIR`.
//!   A configured path that does not exist contributes nothing, which can leave
//!   an empty store; that is a hard error at [`Verifier::new`], and failing
//!   closed is the intended outcome.
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
//!   it does not understand instead of failing the file. The STRICT
//!   [`crate::pem::decode_certificates`] stays strict and stays the parser for
//!   an operator's [`crate::Trust::Roots`] override, where loud failure is the
//!   documented contract;
//! * roots go in through `RootCertStore::add_parsable_certificates`, which drops
//!   the ones webpki rejects rather than refusing the whole store.
//!
//! Tolerance stops there. **If the store ends up empty this arm returns an
//! error rather than a verifier**, so a machine with no CA store fails every
//! HTTPS connection loudly at configuration time instead of quietly trusting
//! nothing — or, far worse, quietly trusting anything.
//!
//! # Revocation: none, here, exactly as before
//!
//! This arm performs NO revocation checking. Neither did
//! `rustls-platform-verifier`'s: its Unix arm is this same
//! `WebPkiServerVerifier`, which ignores a stapled OCSP response unless it was
//! built with CRLs, and it was not. macOS and Windows DO check end-entity
//! revocation. That platform split is pre-existing and preserved — it is
//! written down here so the next reader does not file it as a hole this
//! reimplementation introduced.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::ServerCertVerifier as _;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};

use super::eku_rejected;

/// Bundle files, in the order OpenSSL looks for them. The FIRST that exists
/// wins; the search does not continue past it.
///
/// PER-TARGET, NOT A UNION, and that matters. An earlier version of this file
/// applied one cross-OS union of every distro's paths on EVERY Unix, so aterm
/// scanned a strictly larger set than the platform verifier did — a trust
/// widening in the DEFAULT configuration, with no environment variable
/// involved. These lists are `openssl-probe` 0.2.1's own `#[cfg]` arms
/// (src/lib.rs:137-210) reproduced verbatim, which is what
/// `rustls-native-certs` -> `rustls-platform-verifier` actually consulted.
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

/// Hash directories. EVERY one that exists is read, not just the first.
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

/// The bundle file to read, if any.
///
/// `configured` is `$SSL_CERT_FILE`; it is a parameter rather than an
/// `env::var_os` call inside so [`tests`] can drive both branches without
/// mutating the process environment, which is unsound to do from a test thread.
fn bundle_file(configured: Option<PathBuf>) -> Option<PathBuf> {
    if configured.is_some() {
        // EXCLUSIVE: a set variable suppresses the built-in search entirely,
        // even when it names a path that does not exist. See the module header.
        return configured.filter(|path| path.exists());
    }
    CERTIFICATE_FILES
        .iter()
        .map(PathBuf::from)
        .find(|path| path.exists())
}

/// Every hash directory to scan. `configured` is `$SSL_CERT_DIR`, and it
/// REPLACES the built-in list rather than adding to it — see the module header.
fn bundle_dirs(configured: Option<PathBuf>) -> Vec<PathBuf> {
    if configured.is_some() {
        return configured
            .filter(|path| path.exists())
            .into_iter()
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
    // A file we cannot read, or that is not UTF-8, contributes nothing. The
    // empty-store check in `Verifier::from_ders` is what turns "nothing
    // anywhere" into a loud failure; one unreadable file among hundreds must
    // not be one.
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
            // `fs::metadata` FOLLOWS symlinks, which is the point: an
            // `openssl rehash` directory is almost entirely symlinks. A dangling
            // one just fails here and is skipped.
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
    let env_dir = std::env::var_os("SSL_CERT_DIR").map(PathBuf::from);
    // EITHER variable suppresses BOTH built-in lists — that is what the
    // incumbent's single `CertPaths::from_env()` / `platform::load_native_certs()`
    // branch does. Passing them independently would let `$SSL_CERT_FILE` alone
    // still pull in every built-in hash DIRECTORY.
    let (file, dirs) = if env_file.is_some() || env_dir.is_some() {
        (
            env_file.filter(|p| p.exists()),
            env_dir.filter(|p| p.exists()).into_iter().collect(),
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
    /// `extra_roots` is EMPTY on every shipped path; see [`super::PlatformVerifier`].
    ///
    /// The store is read ONCE, here, rather than per handshake — matching the
    /// incumbent, which built its `WebPkiServerVerifier` in its constructor too.
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
        // Extra anchors are parsed STRICTLY: one that will not parse is a
        // configuration error and is reported, not skipped.
        for root in extra_roots {
            store.add(root)?;
        }
        let (_added, _ignored) =
            store.add_parsable_certificates(system_roots.into_iter().map(CertificateDer::from));
        // FAIL CLOSED. A verifier over an empty root store would reject every
        // chain, which is safe but indistinguishable from a network fault; an
        // empty store is a configuration failure and says so.
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
            // webpki reports a wrong/absent EKU as `InvalidPurpose`; the Apple
            // and Windows arms report the same condition through their own
            // platform codes. Normalising it here is what lets one test assert
            // one outcome on all three.
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
            .unwrap_or_else(|| Arc::new(rustls::crypto::ring::default_provider()))
    }

    #[test]
    fn an_empty_system_store_is_a_loud_failure_not_a_silent_verifier() {
        // THE fail-closed branch. A verifier built over zero roots would reject
        // every chain — safe, but indistinguishable from the network being down,
        // and it hides a broken machine instead of reporting one.
        let error = Verifier::from_ders(Vec::new(), Vec::new(), provider())
            .expect_err("an empty store must not produce a verifier");
        assert!(
            error.to_string().contains("system trust store"),
            "unhelpful error: {error}"
        );
    }

    #[test]
    fn unparseable_system_roots_are_skipped_but_do_not_take_the_store_with_them() {
        // A real distro store legitimately contains certificates webpki refuses.
        // One bad entry must not cost the good ones.
        let good = include_bytes!("../testdata/tls/root.der").to_vec();
        let junk = vec![0u8; 8];
        Verifier::from_ders(Vec::new(), vec![junk, good], provider())
            .expect("one usable root among the rubbish is still a usable store");
    }

    #[test]
    fn an_unusable_extra_anchor_is_an_error_rather_than_a_skip() {
        // The other direction: extra anchors are configuration, not discovery,
        // so a broken one is reported instead of being silently dropped.
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
        // whichever of `CERTIFICATE_FILES` the distro ships. If the host has
        // neither, there is nothing to assert and the test says so rather than
        // failing.
        let file = bundle_file(None);
        let dirs = bundle_dirs(None);
        if file.is_none() && dirs.is_empty() {
            eprintln!("SKIP: this host has no OpenSSL-shaped CA store");
            return;
        }
        let ders = collect_ders(file.as_deref(), &dirs);
        assert!(
            ders.len() > 20,
            "found only {} certificates in {file:?} + {dirs:?}; a real CA store has hundreds",
            ders.len()
        );
        // ...and they are usable as a root store, which is the part that proves
        // the lossy parser produced certificates rather than plausible bytes.
        let verifier = Verifier::from_ders(Vec::new(), ders, provider())
            .expect("the host's own CA store must build a verifier");
        // And the store really validates a real chain: the same captured
        // github.com fixture the Apple arm is checked against, at the same
        // pinned instant.
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
            eprintln!(
                "SKIP (hard assertion only): this host's OpenSSL bundle does not carry the \
                 anchor for the captured github.com chain"
            );
            return;
        }
        // The arming twin: the SAME store must refuse the SAME chain for a name
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
        // `Verifier::new` is the entry point Linux actually uses, and this is
        // what makes it — and `system_root_ders`, including its two environment
        // lookups — run rather than merely compile.
        match Verifier::new(Vec::new(), provider()) {
            Ok(_) => {}
            Err(error) => {
                // Only acceptable outcome on a host with no CA store at all, and
                // even then it must be the LOUD failure, not a silent verifier.
                assert!(
                    error.to_string().contains("system trust store"),
                    "unexpected failure: {error}"
                );
                eprintln!("SKIP: this host has no OpenSSL-shaped CA store");
            }
        }
    }

    /// THE ENVIRONMENT REPLACES THE BUILT-IN SEARCH; IT DOES NOT ADD TO IT.
    ///
    /// This test asserted the opposite until 2026-08-29, and it was green — it
    /// certified a trust-store WIDENING as correct, and its name and comment
    /// would have taught the next reader that the widening was deliberate. The
    /// incumbent (`rustls_native_certs::load_native_certs`, lib.rs:118-125)
    /// consults `CertPaths::from_env()` FIRST and reaches the platform search
    /// only when neither variable is set. An operator pinning one CA must get
    /// exactly that CA.
    #[test]
    fn a_configured_path_replaces_the_built_in_search() {
        // A configured path that does not exist yields NOTHING — it must not
        // fall through to the system store. Failing closed is the point: a
        // stale variable should break TLS loudly, not silently restore every
        // root on the machine.
        let missing = PathBuf::from("/nonexistent/aterm/definitely/not/here.pem");
        assert_eq!(bundle_file(Some(missing.clone())), None);
        assert!(bundle_dirs(Some(missing)).is_empty());
        // And a path that DOES exist wins outright, even though it is not one
        // of the built-in candidates.
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
}
