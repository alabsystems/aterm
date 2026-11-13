// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// atpkg's committed throwaway RSA-4096 PKCS#8 key (its OpenPGP tests sign with it).
pub(crate) const TEST_PKCS8: &[u8] =
    include_bytes!("../../../../atpkg/src/vendor_direct/fixtures/openpgp-test-rsa4096.pk8.der");

/// A service-account JSON key around [`TEST_PKCS8`], exchanged at `token_uri`.
pub(crate) fn key_json(token_uri: &str) -> String {
    let b64 = aterm_codec::base64::encode(TEST_PKCS8).unwrap();
    let mut pem = format!("{BEGIN}\n");
    for line in b64.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(line).unwrap());
        pem.push('\n');
    }
    pem.push_str(END);
    pem.push('\n');
    aterm_json::json!({
        "type": "service_account",
        "project_id": "fake-project",
        "private_key_id": "kid-1",
        "private_key": pem,
        "client_email": "deployer@fake-project.iam.gserviceaccount.com",
        "client_id": "1",
        "token_uri": token_uri,
    })
    .to_string()
}

/// The payload of `jwt` when its RS256 signature verifies under `public` (DER RSAPublicKey).
pub(crate) fn verified(jwt: &str, public: &[u8]) -> Option<aterm_json::Value> {
    let (input, signature) = jwt.rsplit_once('.')?;
    let decode = |part: &str| {
        let standard: String = part
            .chars()
            .map(|c| match c {
                '-' => '+',
                '_' => '/',
                other => other,
            })
            .collect();
        aterm_codec::base64::decode(&standard).ok()
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "an RS256 assertion is RSA, which astream's Ed25519 verify does not cover"
    )]
    ring::signature::UnparsedPublicKey::new(&ring::signature::RSA_PKCS1_2048_8192_SHA256, public)
        .verify(input.as_bytes(), &decode(signature)?)
        .ok()?;
    let (header, claims) = input.split_once('.')?;
    let header: aterm_json::Value = aterm_json::from_slice(&decode(header)?).ok()?;
    if header.get("alg")?.as_str()? != "RS256" {
        return None;
    }
    aterm_json::from_slice(&decode(claims)?).ok()
}

fn public() -> Vec<u8> {
    let pair = ring::signature::RsaKeyPair::from_pkcs8(TEST_PKCS8).unwrap();
    pair.public().as_ref().to_vec()
}

/// The assertion is an RS256 JWT the key's public half verifies, issued by the key's
/// email for the cloud-platform scope, to the token endpoint, for an hour.
/// NEGATIVE CONTROL: one flipped byte of the signed input no longer verifies.
#[test]
fn the_assertion_is_a_jwt_the_public_key_verifies() {
    let key = Key::parse(&Secret::new(
        key_json("https://oauth2.googleapis.com/token").as_bytes(),
    ))
    .unwrap();
    let jwt = String::from_utf8(key.assertion(1_000).unwrap().expose().to_vec()).unwrap();
    assert!(
        !jwt.contains('=') && !jwt.contains('+') && !jwt.contains('/'),
        "base64url"
    );
    let claims = verified(&jwt, &public()).expect("the signature verifies");
    assert_eq!(
        claims,
        aterm_json::json!({
            "iss": "deployer@fake-project.iam.gserviceaccount.com",
            "scope": SCOPE,
            "aud": "https://oauth2.googleapis.com/token",
            "iat": 1_000,
            "exp": 4_600,
        })
    );
    let mut forged = jwt.into_bytes();
    forged[10] ^= 1;
    assert!(verified(&String::from_utf8(forged).unwrap(), &public()).is_none());
    let grant = key.grant(1_000).unwrap();
    assert!(grant.expose().starts_with(
        b"grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion=ey"
    ));
    let max_jwt = String::from_utf8(key.assertion(u64::MAX).unwrap().expose().to_vec()).unwrap();
    let max_claims = verified(&max_jwt, &public()).expect("u64::MAX saturates exp");
    assert_eq!(max_claims["exp"], aterm_json::json!(u64::MAX));
}

/// A file that is not a service-account key is refused by field, and no refusal or
/// `Debug` repeats the key's bytes. NEGATIVE CONTROL: the whole key parses.
#[test]
fn a_broken_key_is_refused_without_saying_its_bytes() {
    let good = key_json("https://oauth2.googleapis.com/token");
    let key = Key::parse(&Secret::new(good.as_bytes())).unwrap();
    assert_eq!(key.project.as_deref(), Some("fake-project"));
    let pem_line = &aterm_codec::base64::encode(TEST_PKCS8).unwrap()[64..128];
    assert!(
        !format!("{key:?}").contains(pem_line),
        "Debug says no key byte"
    );
    let broken = [
        good.replace("service_account", "authorized_user"),
        good.replace("\"client_email\"", "\"x_email\""),
        good.replace("\"token_uri\"", "\"x_uri\""),
        good.replace("PRIVATE KEY", "RSA PRIVATE KEY"),
        good.replace(&pem_line[..8], "!!!!!!!!"),
        good[..good.len() / 2].to_string(),
    ];
    for text in broken {
        let why = Key::parse(&Secret::new(text.as_bytes())).unwrap_err();
        assert!(!why.contains(&pem_line[..16]), "{why}");
        assert!(why.contains("service-account key"), "{why}");
    }
    let scratch = aterm_tempfile::tempdir().unwrap();
    let dir_err = Key::read(scratch.path()).unwrap_err();
    assert!(
        dir_err.contains("cannot read the service-account key"),
        "{dir_err}"
    );
}

#[cfg(unix)]
#[test]
fn key_read_refuses_symlinks() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-hosting-key-link-")
        .tempdir()
        .unwrap();
    let real = dir.path().join("real.json");
    let link = dir.path().join("link.json");
    std::fs::write(&real, key_json("https://oauth2.googleapis.com/token")).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert!(Key::read(&real).is_ok());
    let err = Key::read(&link).unwrap_err();
    assert!(err.contains("cannot read the service-account key"), "{err}");
}
