// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The first-party platform certificate verifier — the OS trust store, called
//! directly.
//!
//! [`crate::tls`] records WHY server certificates are checked against the
//! operating system's trust store and never against a root set compiled into
//! the aterm binary; that argument is unchanged and this module is its
//! implementation. What changed is only WHO makes the call: aterm now speaks to
//! each platform itself instead of routing through `rustls-platform-verifier`
//! (4 packages / 19,534 lines on mac-arm; 10 packages / 29,722 lines across the
//! four measured cells). The trust DECISION is identical — the same
//! Security.framework evaluation, the same `crypt32` chain policy, the same
//! webpki-over-`/etc/ssl/certs` — because it is the same OS making it.
//!
//! # What each platform runs
//!
//! | platform | who decides | revocation |
//! | --- | --- | --- |
//! | macOS / iOS | `SecTrustEvaluateWithError` ([`apple`]) | end-entity, from a stapled OCSP response only (network fetch is off) |
//! | Windows | `CertGetCertificateChain` + `CertVerifyCertificateChainPolicy` ([`windows`]) | end-entity, may retrieve over the network |
//! | Linux, BSD | the distro's on-disk CA store, verified by `rustls`'s own webpki ([`unix`]) | **none** |
//! | wasm32, Android, anything else | nothing — construction fails ([`unsupported`]) | n/a |
//!
//! Every one of those rows describes what `rustls-platform-verifier` did too,
//! with one deliberate exception (macOS forbids network fetching during
//! verification; see [`apple`]) and one deliberate correction: its wasm32 arm
//! quietly fell back to a BUNDLED root set (`webpki-root-certs`), which
//! contradicts the trust model [`crate::tls`] documents. Nothing ships for
//! wasm32, so that was a paper divergence rather than a live one, but the
//! replacement closes it by failing closed instead.
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
//!   that will not encode, an empty root store: all `Err`.
//! * Nothing derived from the peer's chain is ever `unwrap`ped, `expect`ed, or
//!   indexed. It is attacker-controlled input.
//!
//! # The anchor seam, and why it is not in your binary
//!
//! Testing the ACCEPT path offline needs a chain the machine trusts, which needs
//! a way to add an anchor. `rustls-platform-verifier` exposes that as a public
//! `new_with_extra_roots`. Here it is [`PlatformVerifier::new_with_extra_roots`],
//! which is `#[cfg(test)]`: it does not exist in a shipped build, so no caller
//! can reach it and no configuration can turn it on. The seam that lets a caller
//! add trust anchors is the same primitive an attacker wants, and the only thing
//! that needs it is the oracle in [`tests`].
//!
//! [`crate::tls::client_config`] therefore always builds this verifier with an
//! empty anchor set; `platform_verifier_config_carries_no_extra_anchors` in
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

// COMPILED ON EVERY UNIX, INCLUDING APPLE — where it is not the selected arm.
// Two reasons, both about not shipping a module nobody has ever compiled: a
// Mac-only `cargo build` still type-checks it, and `unix::tests` drives its
// store discovery, its tolerant PEM path and its chain math natively against
// `/etc/ssl/cert.pem`, which is the same shape of bundle a Linux box has. What
// that still leaves unproven is named in `unix`'s own header.
#[cfg(all(unix, not(target_os = "android")))]
#[cfg_attr(
    all(target_vendor = "apple", not(test)),
    expect(
        dead_code,
        reason = "on Apple this module is compiled for its type-checking and \
        its tests, but `apple` is the arm that runs"
    )
)]
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
// lands, so a caller — and the differential oracle — sees one set of outcomes.
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
/// deliberately: it is how [`tests`] recognises this outcome on BOTH sides of
/// the differential without either crate exposing the type.
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
fn bad_encoding() -> rustls::Error {
    rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding)
}

/// The certificate is valid but not for the name we asked for.
fn name_mismatch() -> rustls::Error {
    rustls::Error::InvalidCertificate(rustls::CertificateError::NotValidForName)
}

/// No chain to a trusted anchor could be built.
fn unknown_issuer() -> rustls::Error {
    rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer)
}

/// The issuer says this certificate is revoked.
fn revoked() -> rustls::Error {
    rustls::Error::InvalidCertificate(rustls::CertificateError::Revoked)
}

/// See [`EkuError`].
fn eku_rejected() -> rustls::Error {
    rustls::Error::InvalidCertificate(rustls::CertificateError::Other(rustls::OtherError(
        Arc::new(EkuError),
    )))
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
    /// to the system set rather than replacing it, matching the incumbent's
    /// semantics so the two implementations agree on chains that a real system
    /// root would have validated. An operator-facing REPLACE-the-roots override
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
mod tests;
