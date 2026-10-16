// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The first-party platform certificate verifier — the OS trust store, called
//! directly.
//!
//! [`crate::tls`] records WHY server certificates are checked against the
//! operating system's trust store and never against a root set compiled into
//! the aterm binary; this module is its implementation.
//!
//! # What each platform runs
//!
//! | platform | who decides | revocation |
//! | --- | --- | --- |
//! | macOS / iOS | `SecTrustEvaluateWithError` ([`apple`]) | end-entity, from a stapled OCSP response only (network fetch is off) |
//! | Windows | `CertGetCertificateChain` + `CertVerifyCertificateChainPolicy` ([`windows`]) | end-entity, may retrieve over the network |
//! | Linux, BSD | the distro's on-disk CA store, verified by `rustls`'s own webpki (`unix`) | **none** |
//! | wasm32, Android, anything else | nothing — construction fails ([`unsupported`]) | n/a |
//!
//! These match `rustls-platform-verifier` except that macOS forbids network
//! fetching during verification (see [`apple`]) and the remaining targets fail
//! closed instead of falling back to a BUNDLED root set.
//!
//! # Fail closed, and the shapes that enforce it
//!
//! The failure that matters here is not "a good connection was refused", it is
//! "a forged certificate was ACCEPTED". Every naive test exercises the accept
//! path only, so the code is written so that acceptance is hard to reach by
//! accident:
//!
//! * [`PlatformVerifier::verify_server_cert`] can only produce
//!   `ServerCertVerified::assertion()` after an arm returned `Ok(())`. There is
//!   no `match` here whose catch-all accepts, and no arm returns `Ok` except on
//!   an explicit positive result from the OS — `true` from
//!   `SecTrustEvaluateWithError`, `dwError == 0` from the Windows SSL policy, an
//!   `Ok` from `WebPkiServerVerifier`.
//! * An FFI call that fails, a certificate the OS will not parse, a hostname
//!   that will not encode, an empty root store: all `Err`. So is a platform
//!   that never answers: macOS's evaluation runs under a deadline
//!   ([`apple`], `aterm_uds::deadline`) and a `trustd` that does not reply is a refusal
//!   [`is_service_unanswered`] names, never a hang and never an acceptance.
//! * Nothing derived from the peer's chain is ever `unwrap`ped, `expect`ed, or
//!   indexed. It is attacker-controlled input.
//!
//! # The anchor seam, and why it is not in your binary
//!
//! Testing the ACCEPT path offline needs a chain the machine trusts, which needs
//! a way to add an anchor: [`PlatformVerifier::new_with_extra_roots`], which is
//! `#[cfg(test)]`: it does not exist in a shipped build, so no caller
//! can reach it and no configuration can turn it on. The seam that lets a caller
//! add trust anchors is the same primitive an attacker wants, and the only thing
//! that needs it is [`tests`].
//!
//! [`crate::tls::client_config`] therefore always builds this verifier with an
//! empty anchor set; `the_shipped_constructor_trusts_no_fixture_anchor` in
//! [`tests`] asserts exactly that.

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

// ALSO COMPILED IN APPLE TEST BUILDS — where it is not the selected arm, and
// where `unix::tests` drives its store discovery, its tolerant PEM path and its
// chain math natively against `/etc/ssl/cert.pem`, the same shape of bundle a
// Linux box has. A shipped Apple build does not carry it (it is not the arm
// that runs there); the Linux box's own build and the linux-arm cell of
// `verify cells` type-check it for the triples that do. What that still leaves unproven is named in `unix`'s
// own header.
#[cfg(any(
    all(unix, not(target_vendor = "apple"), not(target_os = "android")),
    all(test, target_vendor = "apple")
))]
mod unix;
#[cfg(all(unix, not(target_vendor = "apple"), not(target_os = "android")))]
use unix as imp;

// Everything else — wasm32, Android, and any target that is neither Windows nor
// a non-Android Unix. The arms above are mutually exclusive (Windows is not
// `unix`, and Apple is excluded from the Unix arm), so this is the exact
// complement of their union: no target gets two verifiers, and no target gets
// none.
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

// ---------------------------------------------------------------------------
// Errors
//
// Three of the arms report the same condition through three unrelated
// vocabularies (an Apple `OSStatus`, a Windows `dwError`, a webpki
// `CertificateError`). These constructors are the one place that translation
// lands, so a caller sees one set of outcomes.
//
// What is deliberately NOT translated: anything finer than these. macOS returns
// `errSecCertificateExpired` for a NOT-YET-VALID certificate as well as an
// expired one, and returns different codes for "self-signed leaf" and
// "untrusted root" where webpki returns `UnknownIssuer` for both. Inventing a
// distinction the platform did not make would be inventing information.
// ---------------------------------------------------------------------------

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
///
/// The `Display` text matches `rustls-platform-verifier`'s `EkuError`
/// deliberately: it is how [`tests`] recognises this outcome without the type
/// being exposed.
#[derive(Debug)]
struct EkuError;

impl fmt::Display for EkuError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("certificate had invalid extensions")
    }
}

impl std::error::Error for EkuError {}

/// The peer sent bytes that are not a parseable certificate.
///
/// This is NOT a trust verdict and must never be counted as one: it is reported
/// before any chain evaluation happens.
#[cfg(any(target_vendor = "apple", windows))]
fn bad_encoding() -> rustls::Error {
    rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding)
}

/// The certificate is valid but not for the name we asked for.
#[cfg(any(target_vendor = "apple", windows))]
fn name_mismatch() -> rustls::Error {
    rustls::Error::InvalidCertificate(rustls::CertificateError::NotValidForName)
}

/// No chain to a trusted anchor could be built.
#[cfg(any(target_vendor = "apple", windows))]
fn unknown_issuer() -> rustls::Error {
    rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer)
}

/// The issuer says this certificate is revoked.
#[cfg(any(target_vendor = "apple", windows))]
fn revoked() -> rustls::Error {
    rustls::Error::InvalidCertificate(rustls::CertificateError::Revoked)
}

/// See [`EkuError`].
fn eku_rejected() -> rustls::Error {
    rustls::Error::InvalidCertificate(rustls::CertificateError::Other(rustls::OtherError(
        Arc::new(EkuError),
    )))
}

/// The OS trust service did not answer within the evaluation deadline, or an
/// earlier evaluation has been waiting on it past the suspect age, so the
/// chain was REFUSED without a verdict. Not a judgement of the certificate —
/// the platform never gave one — and not a pass: fail closed.
///
/// A distinct type, so a caller that wants to tell a person "the Mac's
/// certificate service is stuck" apart from "this certificate is bad" asks
/// [`is_service_unanswered`] instead of matching on prose.
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

/// Whether `error` is the platform verifier refusing a connection because the
/// OS trust service did not answer in time (macOS's `trustd`), rather than a
/// verdict on the certificate. Always `false` where there is no such service.
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

// ---------------------------------------------------------------------------
// The verifier
// ---------------------------------------------------------------------------

/// Verifies server certificates against the operating system's trust store.
///
/// Handshake-signature verification is not platform work — it is the crypto
/// provider's — so those three methods delegate to `rustls` exactly as
/// `crates/aterm-net/src/tls.rs:120` already does. Only
/// [`Self::verify_server_cert`] differs per platform.
#[derive(Debug)]
pub struct PlatformVerifier {
    inner: imp::Verifier,
    provider: Arc<CryptoProvider>,
}

impl PlatformVerifier {
    /// A verifier over the platform's own trust anchors, and nothing else.
    ///
    /// # Errors
    ///
    /// A platform that has no OS trust store, or a system store that cannot be
    /// read (on Linux, a store that yields no usable roots at all).
    pub fn new(provider: Arc<CryptoProvider>) -> Result<Self, rustls::Error> {
        Self::build(Vec::new(), provider)
    }

    /// TEST ONLY: the platform's trust anchors PLUS `extra_roots`.
    ///
    /// `#[cfg(test)]` on purpose — see this module's header. Extra anchors ADD
    /// to the system set rather than replacing it, so chains a real system root
    /// validates still validate. An operator-facing REPLACE-the-roots override
    /// already exists and is a different thing: [`crate::Trust::Roots`].
    #[cfg(test)]
    fn new_with_extra_roots(
        extra_roots: Vec<CertificateDer<'static>>,
        provider: Arc<CryptoProvider>,
    ) -> Result<Self, rustls::Error> {
        Self::build(extra_roots, provider)
    }

    /// The one constructor both entry points funnel through, so the shipped
    /// path and the tested path are literally the same code with an empty
    /// `extra_roots` rather than two code paths that resemble each other.
    fn build(
        extra_roots: Vec<CertificateDer<'static>>,
        provider: Arc<CryptoProvider>,
    ) -> Result<Self, rustls::Error> {
        // Every arm takes the same two arguments, so there is no `#[cfg]` fork
        // here: one call, one shape, on all four platforms.
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
        // `to_str` renders an `IpAddress` name in its textual form, which is
        // what the Apple and Windows policies want; the Unix arm gets the
        // `ServerName` itself and does its own IP-vs-DNS matching.
        let server_text = server_name.to_str();
        // FAIL CLOSED on a name there is nothing to check against. A verifier
        // handed an empty name would be asking the platform "is this chain good
        // for "?" — and on macOS an ABSENT name switches name checking off
        // entirely, so a value that could degrade into one is refused here.
        if server_text.is_empty() {
            return Err(invalid_certificate(
                "refusing to verify a certificate against an empty server name",
            ));
        }
        let ocsp_response = (!ocsp_response.is_empty()).then_some(ocsp_response);

        // The ONLY route to `assertion()`. `?` means every arm's `Err` — and
        // every arm's default is `Err` — leaves here as a rejection.
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
