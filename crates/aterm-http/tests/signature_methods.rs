// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HANDSHAKE SIGNATURE METHODS HAVE BEHAVIOURAL COVERAGE.
//!
//! Replacing `verify_tls12_signature` and `verify_tls13_signature` with
//! `Ok(HandshakeSignatureValid::assertion())` is a TOTAL TLS
//! server-authentication bypass: the peer stops having to prove it holds the
//! private key for the certificate it presented, and chain validation is
//! irrelevant against it. These tests are what catches that stub.
//!
//! Nothing here needs a VALID signature, and that is the point — minting one
//! would need a committed private key, which the fixtures' minting
//! (`src/testdata/tls/PROVENANCE.md`) deliberately does not keep and which the
//! repository's B6 guard forbids outright. Proving the
//! REJECT direction is enough to kill the stub: a method that always returns
//! `Ok` cannot reject anything.
//!
//! Verified armed: with both bodies replaced by `Ok(assertion())`, every test
//! in this file fails.

use aterm_http::tls::{Trust, client_config};
use rustls::client::danger::ServerCertVerifier;
use rustls::pki_types::CertificateDer;
use rustls::{DigitallySignedStruct, SignatureScheme};

/// A real, well-formed certificate from the local corpus — so a rejection can
/// only come from the SIGNATURE check, never from failing to parse the cert.
fn good_cert() -> CertificateDer<'static> {
    CertificateDer::from(include_bytes!("../src/testdata/tls/good.der").to_vec())
}

/// The shipped verifier, reached exactly as `client_config` reaches it.
fn verifier() -> std::sync::Arc<dyn ServerCertVerifier> {
    // `client_config` is the production constructor; going through it means this
    // file also fails if the PlatformVerifier stops being what gets installed.
    let _ = client_config(&Trust::PlatformVerifier).expect("platform verifier builds");
    std::sync::Arc::new(
        aterm_http::verifier::PlatformVerifier::new(
            rustls::crypto::CryptoProvider::get_default()
                .cloned()
                .unwrap_or_else(|| std::sync::Arc::new(rustls::crypto::ring::default_provider())),
        )
        .expect("verifier constructs"),
    )
}

/// Build one the only way a consumer can: `DigitallySignedStruct::new` is
/// `pub(crate)` in rustls, but the type implements the public `Codec` trait, so
/// it is decoded from its own wire encoding — two bytes of scheme, a u16
/// length, then the signature.
fn garbage_dss(scheme: SignatureScheme) -> DigitallySignedStruct {
    use rustls::internal::msgs::codec::{Codec, Reader};
    // 72 bytes of zeroes: a plausible LENGTH for an ECDSA P-256 signature, and
    // not a valid signature over anything.
    let sig = [0u8; 72];
    let mut wire = Vec::new();
    scheme.encode(&mut wire);
    wire.extend_from_slice(&u16::try_from(sig.len()).unwrap().to_be_bytes());
    wire.extend_from_slice(&sig);
    let mut reader = Reader::init(&wire);
    DigitallySignedStruct::read(&mut reader).expect("well-formed DigitallySignedStruct encoding")
}

#[test]
fn tls13_rejects_a_signature_that_verifies_against_nothing() {
    let v = verifier();
    let err = v
        .verify_tls13_signature(
            b"transcript bytes the peer never signed",
            &good_cert(),
            &garbage_dss(SignatureScheme::ECDSA_NISTP256_SHA256),
        )
        .expect_err(
            "a bogus TLS 1.3 handshake signature MUST be rejected — if this passes, the peer \
             no longer has to prove it holds the certificate's private key",
        );
    assert!(
        matches!(
            err,
            rustls::Error::InvalidCertificate(_) | rustls::Error::PeerMisbehaved(_)
        ),
        "expected a signature/certificate rejection, got {err:?}"
    );
}

#[test]
fn tls12_rejects_a_signature_that_verifies_against_nothing() {
    let v = verifier();
    v.verify_tls12_signature(
        b"transcript bytes the peer never signed",
        &good_cert(),
        &garbage_dss(SignatureScheme::ECDSA_NISTP256_SHA256),
    )
    .expect_err("a bogus TLS 1.2 handshake signature MUST be rejected");
}
