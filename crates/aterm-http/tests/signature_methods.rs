// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HANDSHAKE SIGNATURE METHODS HAVE BEHAVIOURAL COVERAGE.
//!
//! An adversarial review of the wave that wrote `PlatformVerifier` found that
//! replacing BOTH `verify_tls12_signature` and `verify_tls13_signature` with
//! `Ok(HandshakeSignatureValid::assertion())` left the entire suite green — 68
//! unit + 8 loopback + 17 differential, zero failures. That stub is a TOTAL TLS
//! server-authentication bypass: the peer stops having to prove it holds the
//! private key for the certificate it presented, so anyone able to replay a
//! captured certificate can impersonate the server. Chain validation is
//! irrelevant against it, which is why `verify_server_cert` having a 37-case
//! differential did not help.
//!
//! Nothing here needs a VALID signature, and that is the point — minting one
//! would need a committed private key, which `mint.sh` deliberately does not
//! produce and which the repository's B6 guard forbids outright. Proving the
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

/// THE CONTROL for the two tests above. They assert that something is
/// REJECTED; without this, a verifier that rejected every scheme — including
/// ones a real server legitimately uses — would satisfy them while breaking
/// every handshake. `supported_verify_schemes` must be non-empty and must
/// include the schemes modern servers actually offer.
#[test]
fn the_verifier_still_advertises_real_schemes() {
    let schemes = verifier().supported_verify_schemes();
    assert!(
        !schemes.is_empty(),
        "a verifier advertising no schemes cannot handshake at all"
    );
    for wanted in [
        SignatureScheme::ECDSA_NISTP256_SHA256,
        SignatureScheme::RSA_PSS_SHA256,
    ] {
        assert!(
            schemes.contains(&wanted),
            "{wanted:?} is offered by real servers and must stay supported; got {schemes:?}"
        );
    }
}
