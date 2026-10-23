// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Server certificate verification trust model and `rustls` client configuration.

use std::sync::Arc;

use rustls::pki_types::CertificateDer;
use rustls::{ClientConfig, RootCertStore};

/// How server certificates are verified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trust {
    /// Verify against the operating system's trust store.
    PlatformVerifier,
    /// Verify against only the caller-supplied DER root certificates.
    Roots(Vec<Vec<u8>>),
}

/// Returns the default `ring`-backed `rustls` [`rustls::crypto::CryptoProvider`].
pub(crate) fn provider() -> rustls::crypto::CryptoProvider {
    rustls::crypto::ring::default_provider()
}

/// Installs `provider()` as the process default `CryptoProvider` once.
pub fn init_crypto() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = provider().install_default();
    });
}

/// Build a rustls client configuration for `trust`.
/// # Errors
/// A malformed certificate in caller-supplied roots or none at all, a platform verifier the OS
/// refuses to construct, or protocol versions the provider rejects.
pub fn client_config(trust: &Trust) -> Result<Arc<ClientConfig>, String> {
    init_crypto();
    let provider = rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(provider()));
    let config = match trust {
        Trust::PlatformVerifier => {
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
