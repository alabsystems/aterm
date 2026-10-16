// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The trust model: how a server certificate is verified.
//!
//! * [`Trust::PlatformVerifier`] — the OPERATING SYSTEM's trust store, through
//!   [`crate::verifier::PlatformVerifier`]: Security.framework on macOS/iOS
//!   (verification date pinned to the instant `rustls` supplies, network
//!   fetching disabled), `CertGetCertificateChain` +
//!   `CertVerifyCertificateChainPolicy` under the SSL policy on Windows, and
//!   the distribution's CA store (`$SSL_CERT_FILE`, `$SSL_CERT_DIR`,
//!   `/etc/ssl/certs` and friends) through `rustls`'s webpki on Linux/BSD.
//!   Anywhere else construction FAILS rather than falling back to a bundled
//!   root set. The default, and what every endpoint without a CA file gets.
//! * [`Trust::Roots`] — exactly the certificates in a caller-supplied PEM
//!   bundle, and NOTHING else. An override, not an addition: the platform
//!   roots are not consulted, so a bundle pins the endpoint to its own issuer.

use std::sync::Arc;

use rustls::pki_types::CertificateDer;
use rustls::{ClientConfig, RootCertStore};

/// How server certificates are verified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trust {
    /// Verify against the operating system's trust store.
    PlatformVerifier,
    /// Verify against exactly these DER certificates, REPLACING the platform
    /// roots (a caller's explicit per-endpoint override).
    Roots(Vec<Vec<u8>>),
}

/// THE crypto provider: the one place this crate names a rustls
/// `CryptoProvider` implementation. Today it is rustls's `ring` provider, and
/// ring is the bridge half of the KEEP pair (docs/AUDIT-third-party-2026-10-15.md):
/// replacing it is a change to this function, not to its callers.
pub(crate) fn provider() -> rustls::crypto::CryptoProvider {
    rustls::crypto::ring::default_provider()
}

/// Install `provider()` as the process default, once.
///
/// Idempotent, and it tolerates another component having already installed one
/// — `aterm-net` installs the same provider for the L3 drive, and whichever
/// runs first wins with an identical result.
pub fn init_crypto() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = provider().install_default();
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
        .unwrap_or_else(|| Arc::new(provider()));
    let config = match trust {
        Trust::PlatformVerifier => {
            // `PlatformVerifier::new` is the ONLY constructor reachable from
            // here: the extra-trust-anchor seam its tests need is
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
    fn init_crypto_is_idempotent() {
        init_crypto();
        init_crypto();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }
}
