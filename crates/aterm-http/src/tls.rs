// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TRUST MODEL — and the fact that retiring `ureq` did not change it.
//!
//! `crates/aterm-gui/Cargo.toml` asked `ureq` for its `platform-verifier`
//! feature ON PURPOSE: a server certificate is checked against the OPERATING
//! SYSTEM's trust store — Keychain on macOS, the system store elsewhere — and
//! never against a root set compiled into the aterm binary. That choice is why
//! `security-framework` and `core-foundation` were in the dependency graph at
//! all.
//!
//! Retiring `ureq` removes ureq's own HTTP/1.1 stack (`ureq-proto`, `http`,
//! `bytes`, `httparse`, `utf8-zero`, `percent-encoding`) AND the bundled root
//! set it carried for the non-platform case (`webpki-roots`). It deliberately
//! does NOT remove platform verification: a bundled root set would have been a
//! real, owner-visible security change, and it was not made here.
//!
//! # The implementation is now FIRST-PARTY. The trust model is not new.
//!
//! [`Trust::PlatformVerifier`] used to be `rustls_platform_verifier::Verifier`.
//! It is now [`crate::verifier::PlatformVerifier`], which makes the same
//! operating-system calls itself:
//!
//! * **macOS / iOS** — `SecPolicyCreateSSL` + `SecTrustCreateWithCertificates` +
//!   `SecTrustEvaluateWithError`, with the verification date pinned to the
//!   instant `rustls` supplies and network fetching explicitly disabled;
//! * **Windows** — `CertGetCertificateChain` + `CertVerifyCertificateChainPolicy`
//!   under the SSL chain policy;
//! * **Linux, BSD** — the distribution's on-disk CA store (`$SSL_CERT_FILE`,
//!   `$SSL_CERT_DIR`, `/etc/ssl/certs` and friends), verified by `rustls`'s own
//!   webpki. No package is added for any of the three;
//! * **anything else** (wasm32, Android) — construction FAILS, rather than
//!   silently falling back to a bundled root set the way the retired crate's
//!   wasm32 arm did.
//!
//! That is the SAME trust decision, made by the same operating system, reached
//! without 4 packages / 19,534 lines of third-party code on mac-arm (10 / 29,722
//! across the four measured cells). The retired crate is kept as a
//! `[dev-dependencies]` oracle and every fixture chain is driven through both
//! implementations; see `crate::verifier::tests`.
//!
//! Two trust modes, matching what the previous client exposed one-for-one:
//!
//! * [`Trust::PlatformVerifier`] — the OS trust store. The default, and what
//!   every endpoint without an explicit CA file gets.
//! * [`Trust::Roots`] — exactly the certificates in an operator-configured PEM
//!   bundle, and NOTHING else. This is an override, not an addition: the
//!   platform roots are not consulted, so a bundle pins the provider to its own
//!   issuer. (`ureq::tls::RootCerts::Specific` had the same replace-not-extend
//!   semantics, so this preserves the configured behaviour.)

use std::sync::Arc;

use rustls::pki_types::CertificateDer;
use rustls::{ClientConfig, RootCertStore};

/// How server certificates are verified.
#[derive(Clone, Debug)]
pub enum Trust {
    /// Verify against the operating system's trust store.
    PlatformVerifier,
    /// Verify against exactly these DER certificates, REPLACING the platform
    /// roots (an operator's explicit per-endpoint override).
    Roots(Vec<Vec<u8>>),
}

/// Install the `ring` crypto provider as the process default, once.
///
/// Idempotent, and it tolerates another component having already installed one
/// — `aterm-net` installs the same provider for the L3 drive, and whichever
/// runs first wins with an identical result.
pub fn init_crypto() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Build a rustls client configuration for `trust`.
///
/// # Errors
///
/// A malformed certificate in a configured bundle, or a platform verifier the
/// OS refuses to construct.
pub fn client_config(trust: &Trust) -> Result<Arc<ClientConfig>, String> {
    init_crypto();
    let provider = rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::ring::default_provider()));
    let config = match trust {
        Trust::PlatformVerifier => {
            // `PlatformVerifier::new` is the ONLY constructor reachable from
            // here: the extra-trust-anchor seam its differential test needs is
            // `#[cfg(test)]`, so this path cannot be given anchors beyond the
            // operating system's own, and the compiler is what enforces that.
            let verifier = crate::verifier::PlatformVerifier::new(Arc::clone(&provider))
                .map_err(|error| format!("platform certificate verifier unavailable: {error}"))?;
            ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .map_err(|error| format!("TLS protocol versions rejected: {error}"))?
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(verifier))
                .with_no_client_auth()
        }
        Trust::Roots(ders) => {
            let mut store = RootCertStore::empty();
            for (index, der) in ders.iter().enumerate() {
                store
                    .add(CertificateDer::from(der.clone()))
                    .map_err(|error| {
                        format!("CA bundle certificate {} is not usable: {error}", index + 1)
                    })?;
            }
            if store.is_empty() {
                return Err("CA bundle yielded no usable certificates".to_owned());
            }
            ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .map_err(|error| format!("TLS protocol versions rejected: {error}"))?
                .with_root_certificates(store)
                .with_no_client_auth()
        }
    };
    Ok(Arc::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_platform_verifier_is_the_default_trust_model() {
        // The whole point of the retirement: the OS trust store is still the
        // path an endpoint with no configured CA file takes.
        let config = client_config(&Trust::PlatformVerifier).expect("platform verifier builds");
        assert!(!config.alpn_protocols.iter().any(Vec::is_empty));
    }

    #[test]
    fn a_bundle_of_garbage_is_rejected_rather_than_silently_trusting_nothing() {
        let error = client_config(&Trust::Roots(vec![vec![0u8; 8]])).unwrap_err();
        assert!(error.contains("CA bundle"), "{error}");
    }

    #[test]
    fn an_empty_root_set_is_rejected() {
        assert!(client_config(&Trust::Roots(Vec::new())).is_err());
    }

    #[test]
    fn the_platform_arm_does_not_consult_an_operator_bundle_and_vice_versa() {
        // The two modes are separate trust decisions and must not be able to
        // leak into one another: `Roots` REPLACES the platform anchors, and
        // `PlatformVerifier` adds none of its own.
        let platform = client_config(&Trust::PlatformVerifier).expect("platform verifier builds");
        let bundle = client_config(&Trust::Roots(vec![
            include_bytes!("testdata/tls/root.der").to_vec(),
        ]))
        .expect("a one-root bundle builds");
        assert!(!Arc::ptr_eq(&platform, &bundle));
    }

    #[test]
    fn init_crypto_is_idempotent() {
        init_crypto();
        init_crypto();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }
}
