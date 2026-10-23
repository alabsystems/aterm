// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Behavioural coverage for the handshake signature methods.

use aterm_http::tls::{Trust, client_config};
use rustls::client::danger::ServerCertVerifier;
use rustls::pki_types::CertificateDer;
use rustls::{DigitallySignedStruct, SignatureScheme};

fn good_cert() -> CertificateDer<'static> {
    CertificateDer::from(include_bytes!("../src/testdata/tls/good.der").to_vec())
}

fn verifier() -> std::sync::Arc<dyn ServerCertVerifier> {
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

fn garbage_dss(scheme: SignatureScheme) -> DigitallySignedStruct {
    use rustls::internal::msgs::codec::{Codec, Reader};
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
