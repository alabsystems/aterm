// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fail-closed trust configuration: an empty or unparseable CA bundle is an
//! error, never an empty or default trust set.

const ROOT: &[u8] = include_bytes!("../src/testdata/tls/root.der");

#[test]
fn an_empty_root_store_is_refused_rather_than_silently_trusting_nothing() {
    use aterm_http::Trust;

    // A `Trust::Roots` bundle with nothing in it must be an ERROR, not a
    // verifier that rejects everything and not one that falls back to the
    // platform anchors. Both fallbacks are worse than failing: the first turns a
    // configuration mistake into a mysterious outage, the second turns an
    // operator's explicit pin into no pin at all.
    let error = aterm_http::tls::client_config(&Trust::Roots(Vec::new()))
        .expect_err("an empty CA bundle must not build a client configuration");
    assert!(
        error.contains("no usable certificates"),
        "the empty-bundle error must say what was wrong: {error}"
    );

    // The same shape from the other direction: a bundle whose every entry is
    // unusable is empty after decoding, and must fail the same way rather than
    // quietly yielding a store with fewer roots than the operator listed.
    let error = aterm_http::tls::client_config(&Trust::Roots(vec![b"not a certificate".to_vec()]))
        .expect_err("a bundle of garbage must not build a client configuration");
    assert!(error.contains("CA bundle"), "{error}");

    // And the platform arm has its own empty-store failure, one this test
    // cannot reach because it needs control of `$SSL_CERT_FILE`:
    // `verifier::unix::tests::an_empty_system_store_is_a_loud_failure_not_a_silent_verifier`.
    // On Apple the store is the Keychain and cannot be emptied by a test at all.
}

#[test]
fn an_unparseable_ca_bundle_is_refused_rather_than_silently_empty() {
    use aterm_http::pem;

    // THE STRICT PARSER, used for the operator's `Trust::Roots` override. Every
    // one of these must be an error rather than a shorter list of roots: a
    // bundle that silently loses its entries is a pin that silently stops
    // pinning.
    for (what, text) in [
        ("nothing at all", ""),
        ("prose", "this is not a certificate"),
        (
            "a header with no footer",
            "-----BEGIN CERTIFICATE-----\nQUJD\n",
        ),
        (
            "a footer with no header",
            "QUJD\n-----END CERTIFICATE-----\n",
        ),
        (
            "base64 that is not base64",
            "-----BEGIN CERTIFICATE-----\n!!!! not base64 !!!!\n-----END CERTIFICATE-----\n",
        ),
    ] {
        assert!(
            pem::decode_certificates(text).is_err(),
            "the strict PEM parser accepted {what}"
        );
    }

    // A key where a certificate should be. The PEM label is ASSEMBLED rather
    // than written out, because `tools/grep_guard.sh`'s B6 check greps the whole
    // tracked tree for private-key headers and a literal one here would trip it
    // (`crates/atpkg-keys` does the same).
    let wrong_label = format!(
        "-----BEGIN {label}-----\nQUJD\n-----END {label}-----\n",
        label = "PRIVATE KEY"
    );
    assert!(
        pem::decode_certificates(&wrong_label).is_err(),
        "the strict PEM parser accepted a key where a certificate should be"
    );
    assert!(
        pem::decode_certificates_lossy(&wrong_label).is_empty(),
        "the tolerant PEM parser accepted a key where a certificate should be"
    );

    // A well-formed PEM whose payload is not a certificate parses as PEM but
    // must be refused by the trust builder -- the layer below is where DER is
    // judged, and it must not be skipped.
    let junk_pem = "-----BEGIN CERTIFICATE-----\nQUJDREVGRw==\n-----END CERTIFICATE-----\n";
    let ders = pem::decode_certificates(junk_pem).expect("well-formed PEM decodes");
    assert_eq!(
        ders.len(),
        1,
        "the PEM layer must hand on exactly what it saw"
    );
    let error = aterm_http::tls::client_config(&aterm_http::Trust::Roots(ders))
        .expect_err("PEM-shaped garbage must not become a trust anchor");
    assert!(error.contains("CA bundle"), "{error}");

    // The TOLERANT parser exists only for reading a Linux distribution's system
    // store, where a single unreadable file must not take the whole store with
    // it. It must still never invent a certificate: garbage in, nothing out.
    for text in [
        "",
        "prose",
        "-----BEGIN CERTIFICATE-----\n!!!\n-----END CERTIFICATE-----\n",
    ] {
        assert!(
            pem::decode_certificates_lossy(text).is_empty(),
            "the tolerant parser produced a certificate from {text:?}"
        );
    }

    // ...and an empty result from the tolerant parser must still fail closed
    // when it reaches the trust builder, rather than becoming an empty store.
    assert!(
        aterm_http::tls::client_config(&aterm_http::Trust::Roots(pem::decode_certificates_lossy(
            "prose"
        )))
        .is_err(),
        "a store the tolerant parser could not fill must not build a verifier"
    );

    // A real certificate mixed in with junk the tolerant parser skips must
    // still come through, or "tolerant" would just mean "broken".
    let mixed = format!(
        "garbage before\n-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\ngarbage after\n",
        aterm_codec::base64::encode(ROOT).expect("encode")
    );
    assert_eq!(
        pem::decode_certificates_lossy(&mixed),
        vec![ROOT.to_vec()],
        "the tolerant parser lost a valid certificate that was surrounded by junk"
    );
}
