// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The first-party platform certificate verifier: the OS trust store, called directly.

use std::fmt;
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

#[cfg(target_vendor = "apple")]
mod apple;
#[cfg(target_vendor = "apple")]
use apple as imp;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

#[cfg(any(
    all(unix, not(target_vendor = "apple"), not(target_os = "android")),
    all(test, target_vendor = "apple")
))]
mod unix;
#[cfg(all(unix, not(target_vendor = "apple"), not(target_os = "android")))]
use unix as imp;

#[cfg(not(any(
    target_vendor = "apple",
    windows,
    all(unix, not(target_vendor = "apple"), not(target_os = "android"))
)))]
mod unsupported;
#[cfg(not(any(
    target_vendor = "apple",
    windows,
    all(unix, not(target_vendor = "apple"), not(target_os = "android"))
)))]
use unsupported as imp;

/// A rejection carrying a platform-specific explanation.
#[derive(Debug)]
struct PlatformRejection(String);

impl fmt::Display for PlatformRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for PlatformRejection {}

/// The end-entity certificate's extended key usage does not permit server
/// authentication (or, on Apple, is absent where Apple requires it).
#[derive(Debug)]
struct EkuError;

impl fmt::Display for EkuError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("certificate had invalid extensions")
    }
}

impl std::error::Error for EkuError {}

/// The peer sent bytes that are not a parseable certificate. Reported before
/// any chain evaluation: not a trust verdict.
#[cfg(any(target_vendor = "apple", windows))]
const fn bad_encoding() -> rustls::Error {
    rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding)
}

/// The certificate is valid but not for the name we asked for.
#[cfg(any(target_vendor = "apple", windows))]
const fn name_mismatch() -> rustls::Error {
    rustls::Error::InvalidCertificate(rustls::CertificateError::NotValidForName)
}

/// No chain to a trusted anchor could be built.
#[cfg(any(target_vendor = "apple", windows))]
const fn unknown_issuer() -> rustls::Error {
    rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer)
}

/// The issuer says this certificate is revoked.
#[cfg(any(target_vendor = "apple", windows))]
const fn revoked() -> rustls::Error {
    rustls::Error::InvalidCertificate(rustls::CertificateError::Revoked)
}

/// See [`EkuError`].
fn eku_rejected() -> rustls::Error {
    rustls::Error::InvalidCertificate(rustls::CertificateError::Other(rustls::OtherError(
        Arc::new(EkuError),
    )))
}

/// The OS trust service did not answer within the evaluation deadline, or an
/// earlier evaluation has waited on it past the suspect age, so the chain was
/// refused without a verdict.
#[cfg(target_vendor = "apple")]
#[derive(Debug)]
struct ServiceUnanswered;

#[cfg(target_vendor = "apple")]
impl fmt::Display for ServiceUnanswered {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "macOS's certificate service (trustd) is not answering; TLS connections are refused \
             until it answers",
        )
    }
}

#[cfg(target_vendor = "apple")]
impl std::error::Error for ServiceUnanswered {}

/// See [`ServiceUnanswered`]. `rustls::Error::Other`, not `InvalidCertificate`:
/// nothing is known to be wrong with the certificate, and rustls would print
/// the latter as "invalid peer certificate".
#[cfg(target_vendor = "apple")]
fn service_unanswered() -> rustls::Error {
    rustls::Error::Other(rustls::OtherError(Arc::new(ServiceUnanswered)))
}

/// Whether `error` is the platform verifier refusing because the OS trust
/// service (macOS's `trustd`) did not answer in time, rather than a verdict on
/// the certificate.
#[must_use]
pub fn is_service_unanswered(error: &rustls::Error) -> bool {
    #[cfg(target_vendor = "apple")]
    {
        matches!(error, rustls::Error::Other(other) if other.0.is::<ServiceUnanswered>())
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        let _ = error;
        false
    }
}

/// A rejection the platform explained in its own words.
fn invalid_certificate(reason: impl Into<String>) -> rustls::Error {
    rustls::Error::InvalidCertificate(rustls::CertificateError::Other(rustls::OtherError(
        Arc::new(PlatformRejection(reason.into())),
    )))
}

/// Verifies server certificates against the operating system's trust store.
#[derive(Debug)]
pub struct PlatformVerifier {
    inner: imp::Verifier,
    provider: Arc<CryptoProvider>,
}

impl PlatformVerifier {
    /// A verifier over the platform's own trust anchors, and nothing else.
    /// # Errors
    /// A platform that has no OS trust store, or a system store that cannot be read (on Linux, a
    /// store that yields no usable roots at all).
    pub fn new(provider: Arc<CryptoProvider>) -> Result<Self, rustls::Error> {
        Self::build(Vec::new(), provider)
    }

    /// Test only: the platform's trust anchors plus
    /// `extra_roots`, which add to the system set rather than replace it.
    #[cfg(test)]
    fn new_with_extra_roots(
        extra_roots: Vec<CertificateDer<'static>>,
        provider: Arc<CryptoProvider>,
    ) -> Result<Self, rustls::Error> {
        Self::build(extra_roots, provider)
    }

    /// The one constructor both entry points funnel through, so the shipped
    /// path is the tested path with an empty `extra_roots`.
    fn build(
        extra_roots: Vec<CertificateDer<'static>>,
        provider: Arc<CryptoProvider>,
    ) -> Result<Self, rustls::Error> {
        let inner = imp::Verifier::new(extra_roots, Arc::clone(&provider))?;
        Ok(Self { inner, provider })
    }
}

impl ServerCertVerifier for PlatformVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let server_text = server_name.to_str();
        if server_text.is_empty() {
            return Err(invalid_certificate(
                "refusing to verify a certificate against an empty server name",
            ));
        }
        let ocsp_response = (!ocsp_response.is_empty()).then_some(ocsp_response);

        self.inner.verify(
            end_entity,
            intermediates,
            server_name,
            &server_text,
            ocsp_response,
            now,
        )?;
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod test_platform;
#[cfg(test)]
mod tests;
