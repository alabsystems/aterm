// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

const KEYS_JSON: &[u8] = include_bytes!("fixtures/npm-registry-keys.json");
const LINUX_X64: &[u8] = include_bytes!("fixtures/grok-1.0.70-npm-linux-x64.json");
const LINUX_ARM64: &[u8] = include_bytes!("fixtures/grok-1.0.70-npm-linux-arm64.json");
const DARWIN_ARM64: &[u8] = include_bytes!("fixtures/grok-1.0.70-npm-darwin-arm64.json");
/// 2026-10-24.
const NOW: i64 = 1_792_800_000;

fn want(npm: &str) -> (String, String) {
    (
        format!("@xai-official/grok-{npm}"),
        format!("https://registry.npmjs.org/@xai-official/grok-{npm}/-/grok-{npm}-1.0.70.tgz"),
    )
}

fn expected<'a>(name: &'a str, tarball: &'a str, os: &'a str, cpu: &'a str) -> Expected<'a> {
    Expected {
        name,
        version: Version::parse("1.0.70").unwrap(),
        tarball,
        os,
        cpu,
    }
}

fn edit(doc: &[u8], from: &str, to: &str) -> Vec<u8> {
    let text = std::str::from_utf8(doc).unwrap();
    assert_eq!(text.matches(from).count(), 1, "{from}");
    text.replace(from, to).into_bytes()
}

/// The shipped key is the registry's: the one unexpired key the registry served
/// (`npm-registry-keys.json`), its point the SPKI past the P-256 prefix and its keyid the
/// OpenSSH fingerprint of that point; the key that expired in 2025 is not shipped.
#[test]
fn the_shipped_key_is_the_registrys_unexpired_one() {
    const SPKI_P256: [u8; 26] = [
        0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08,
        0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
    ];
    let doc = super::super::resolve::strict_json(KEYS_JSON).unwrap();
    let Some(aterm_json::Value::Array(served)) = doc.get("keys") else {
        panic!("the fixture lists keys");
    };
    let mut unexpired = Vec::new();
    for key in served {
        let field = |k: &str| key.get(k).and_then(aterm_json::Value::as_str);
        assert_eq!(field("keytype"), Some("ecdsa-sha2-nistp256"));
        assert_eq!(field("scheme"), Some("ecdsa-sha2-nistp256"));
        let spki = aterm_codec::base64::decode_strict(field("key").unwrap().as_bytes()).unwrap();
        assert_eq!(&spki[..26], &SPKI_P256[..], "a P-256 SPKI");
        let point: [u8; 65] = spki[26..].try_into().unwrap();
        assert_eq!(
            keyid_of(&point),
            field("keyid").unwrap(),
            "keyid is the fingerprint"
        );
        if key.get("expires") == Some(&aterm_json::Value::Null) {
            unexpired.push((field("keyid").unwrap().to_string(), point));
        }
    }
    assert_eq!(served.len(), 2);
    let shipped: Vec<(String, [u8; 65])> = NPM_REGISTRY_KEYS
        .iter()
        .map(|k| (k.keyid.to_string(), k.point))
        .collect();
    assert_eq!(shipped, unexpired);
    assert!(NPM_REGISTRY_KEYS.iter().all(|k| k.not_after.is_none()));
}

/// The real 1.0.70 documents verify under the shipped key on each platform the lane
/// serves, to the sha512 their integrity names (each carries two signatures, both under
/// that key).
#[test]
fn the_real_version_documents_verify_under_the_shipped_key() {
    for (doc, npm, os, cpu, integrity) in [
        (
            LINUX_X64,
            "linux-x64",
            "linux",
            "x64",
            "ac496c872733ffa4a03d2922146b4a1c0e2f495388f2a43a7748c57aee08f768\
             5708c7efc603223af352cee0619b63fc25dfb8acd180b72db35b45709a01d278",
        ),
        (
            LINUX_ARM64,
            "linux-arm64",
            "linux",
            "arm64",
            "ac7d137d6393fc26bfb0132d00927e189f54c9996bfd3913a881eaf052fba3e6\
             53cc93015af0afe34242e13fd7bb0119dd6326c4e1c688f6a9ec7f41e126c34b",
        ),
        (
            DARWIN_ARM64,
            "darwin-arm64",
            "darwin",
            "arm64",
            "4efcd9e1be98c2988aed4c9b246278017c400fb7f3f18f75d90d2db18e959280\
             c5460a7121458175ba7a031f080899f54447fbaa7b0b71146eca268e4f025b9a",
        ),
    ] {
        let (name, tarball) = want(npm);
        let got = verify_dist(
            doc,
            &expected(&name, &tarball, os, cpu),
            NPM_REGISTRY_KEYS,
            NOW,
        )
        .unwrap_or_else(|e| panic!("{npm}: {e}"));
        assert_eq!(got.alg(), DigestAlg::Sha512);
        assert_eq!(got.as_str(), integrity, "{npm}");
    }
}

fn refusal(doc: &[u8], keys: &[NpmKey]) -> String {
    let (name, tarball) = want("linux-x64");
    verify_dist(doc, &expected(&name, &tarball, "linux", "x64"), keys, NOW).unwrap_err()
}

const FIRST_SIG: &str = "MEYCIQCUd51BnTs1QFK8iCXGx4zyss7hGUpSmCK7LdW0q4m4LgIhAKU1ICFaPwxr\
                         ItCHbgTVha6EV566gITf+xp7z9ubTZ2o";
const SECOND_SIG: &str = "MEYCIQDavMCpkB8fhmn91a05PjBlEpTpEfj7s9UBngUh6OS4ugIhAJLZ8clbbYLN\
                          +XT8GiRTMSXHgBxBVWA6Se5PON4BGFfo";
const INTEGRITY: &str = "sha512-rElshycz/6SgPSkiFGtKHA4vSVOI8qQ6d0jFeu4I92hXCMfvxgMiOvNSzuB\
                         hm2P8Jd+4rNGAty2zW0VwmgHSeA==";
const KEYID: &str = "SHA256:DhQ8wR5APBvFHLF/+Tc+AYvPOdTpcIDqOhxsBHRwC7U";

/// Every refusal on the real document: each edit of linux-x64's 1.0.70 document that
/// changes what it vouches for, how it is signed, or what it is, is refused, naming why.
#[test]
fn every_edit_of_the_real_document_is_refused() {
    let other_key = NpmKey {
        keyid: "SHA256:jl3bwswu80PjjokCgh0o2w5c2U4LhQAE57gj9cz1kzA",
        ..NPM_REGISTRY_KEYS[0]
    };
    let renamed = |from: &str| -> Vec<u8> {
        edit(
            &edit(
                LINUX_X64,
                &format!("{FIRST_SIG}\",\"keyid\":\"{KEYID}"),
                &format!("{FIRST_SIG}\",\"keyid\":\"{from}"),
            ),
            &format!("{SECOND_SIG}\",\"keyid\":\"{KEYID}"),
            &format!("{SECOND_SIG}\",\"keyid\":\"{from}"),
        )
    };
    let flipped_sig = {
        let mut der = aterm_codec::base64::decode_strict(FIRST_SIG.as_bytes()).unwrap();
        *der.last_mut().unwrap() ^= 1;
        aterm_codec::base64::encode(&der).unwrap()
    };
    let other_integrity = {
        let mut bytes =
            aterm_codec::base64::decode_strict(&INTEGRITY.as_bytes()["sha512-".len()..]).unwrap();
        bytes[0] ^= 1;
        format!("sha512-{}", aterm_codec::base64::encode(&bytes).unwrap())
    };
    for (doc, keys, why) in [
        (
            edit(
                LINUX_X64,
                &format!("\"integrity\":\"{INTEGRITY}\""),
                &format!("\"integrity\":\"{other_integrity}\""),
            ),
            NPM_REGISTRY_KEYS,
            "does not verify",
        ),
        (
            edit(LINUX_X64, FIRST_SIG, &flipped_sig),
            NPM_REGISTRY_KEYS,
            "does not verify",
        ),
        (
            edit(
                LINUX_X64,
                "\"name\":\"@xai-official/grok-linux-x64\"",
                "\"name\":\"@xai-official/grok-linux-arm64\"",
            ),
            NPM_REGISTRY_KEYS,
            "names the package",
        ),
        (
            edit(
                LINUX_X64,
                "\"version\":\"1.0.70\"",
                "\"version\":\"1.0.69\"",
            ),
            NPM_REGISTRY_KEYS,
            "is version",
        ),
        (
            edit(
                LINUX_X64,
                "grok-linux-x64/-/grok-linux-x64-1.0.70.tgz",
                "grok-linux-x64/-/grok-linux-x64-1.0.69.tgz",
            ),
            NPM_REGISTRY_KEYS,
            "names the tarball",
        ),
        (
            edit(
                LINUX_X64,
                "https://registry.npmjs.org/@xai-official/grok-linux-x64/-/",
                "https://registry.npmjs.com/@xai-official/grok-linux-x64/-/",
            ),
            NPM_REGISTRY_KEYS,
            "names the tarball",
        ),
        (
            edit(
                LINUX_X64,
                "\"os\":[\"linux\"]",
                "\"os\":[\"linux\",\"darwin\"]",
            ),
            NPM_REGISTRY_KEYS,
            "not for os linux alone",
        ),
        (
            edit(LINUX_X64, "\"os\":[\"linux\"]", "\"os\":[\"linux\",123]"),
            NPM_REGISTRY_KEYS,
            "not for os linux alone",
        ),
        (
            edit(LINUX_X64, "\"cpu\":[\"x64\"]", "\"cpu\":[\"arm64\"]"),
            NPM_REGISTRY_KEYS,
            "not for cpu x64 alone",
        ),
        (
            edit(LINUX_X64, "\"cpu\":[\"x64\"]", "\"cpu\":[\"x64\",null]"),
            NPM_REGISTRY_KEYS,
            "not for cpu x64 alone",
        ),
        (
            edit(
                LINUX_X64,
                &format!("\"integrity\":\"{INTEGRITY}\""),
                &format!("\"integrity\":\"{INTEGRITY} {INTEGRITY}\""),
            ),
            NPM_REGISTRY_KEYS,
            "no single `sha512-` integrity",
        ),
        (
            edit(
                LINUX_X64,
                &format!("\"integrity\":\"{INTEGRITY}\""),
                "\"integrity\":\"sha1-SGbMnJk615LEE5vtn4VDXKZ9EFU=\"",
            ),
            NPM_REGISTRY_KEYS,
            "no single `sha512-` integrity",
        ),
        (
            edit(
                LINUX_X64,
                &format!("\"integrity\":\"{INTEGRITY}\""),
                &format!("\"integrity\":\"{}\"", INTEGRITY.trim_end_matches('=')),
            ),
            NPM_REGISTRY_KEYS,
            "no single `sha512-` integrity",
        ),
        (
            edit(
                LINUX_X64,
                &format!(
                    "\"signatures\":[{{\"sig\":\"{FIRST_SIG}\",\"keyid\":\"{KEYID}\"}},{{\"sig\":\"{SECOND_SIG}\",\"keyid\":\"{KEYID}\"}}]"
                ),
                "\"signatures\":[]",
            ),
            NPM_REGISTRY_KEYS,
            "carries no registry signature",
        ),
        (
            edit(
                LINUX_X64,
                &format!(
                    ",\"signatures\":[{{\"sig\":\"{FIRST_SIG}\",\"keyid\":\"{KEYID}\"}},{{\"sig\":\"{SECOND_SIG}\",\"keyid\":\"{KEYID}\"}}]"
                ),
                "",
            ),
            NPM_REGISTRY_KEYS,
            "carries no registry signature",
        ),
        (
            renamed("SHA256:unknownkeyid"),
            NPM_REGISTRY_KEYS,
            "signed only by registry keys aterm does not ship (SHA256:unknownkeyid)",
        ),
        (
            LINUX_X64.to_vec(),
            &[other_key][..],
            "signed only by registry keys aterm does not ship (SHA256:DhQ8",
        ),
        (
            edit(
                LINUX_X64,
                "\"version\":\"1.0.70\"}",
                "\"version\":\"1.0.70\",\"name\":\"x\"}",
            ),
            NPM_REGISTRY_KEYS,
            "not strict JSON",
        ),
        (
            edit(LINUX_X64, "\"_npmVersion\"", "\"_npmVersion\"\"\""),
            NPM_REGISTRY_KEYS,
            "not strict JSON",
        ),
        (
            edit(
                LINUX_X64,
                &format!("\"sig\":\"{FIRST_SIG}\""),
                "\"sig\":\"not base64!\"",
            ),
            NPM_REGISTRY_KEYS,
            "not a keyid and a base64 signature",
        ),
    ] {
        let got = refusal(&doc, keys);
        assert!(got.contains(why), "want {why:?}, got {got}");
    }
    let mut point = NPM_REGISTRY_KEYS[0].point;
    point[64] ^= 1;
    let misnamed = NpmKey {
        point,
        ..NPM_REGISTRY_KEYS[0]
    };
    assert!(
        refusal(LINUX_X64, &[misnamed]).contains("aterm does not ship (SHA256:DhQ8"),
        "a key whose point is not the one its keyid names is no key of the registry's"
    );
    let expired = NpmKey {
        not_after: Some(NOW - 1),
        ..NPM_REGISTRY_KEYS[0]
    };
    assert!(
        refusal(LINUX_X64, &[expired]).contains("only under expired registry keys"),
        "a key past its end vouches for nothing"
    );
    let later = NpmKey {
        not_after: Some(NOW),
        ..NPM_REGISTRY_KEYS[0]
    };
    let (name, tarball) = want("linux-x64");
    assert!(
        verify_dist(
            LINUX_X64,
            &expected(&name, &tarball, "linux", "x64"),
            &[later],
            NOW
        )
        .is_ok(),
        "a key still vouches on its last second"
    );
}

/// An unknown key is ignored only beside a shipped key's signature that verifies: a
/// document that adds one keeps verifying, and the shipped key's bad signature beside an
/// unknown key's is still refused.
#[test]
fn an_unknown_key_is_ignored_only_beside_a_shipped_one_that_verifies() {
    let added = edit(
        LINUX_X64,
        &format!("{{\"sig\":\"{SECOND_SIG}\",\"keyid\":\"{KEYID}\"}}"),
        &format!(
            "{{\"sig\":\"{SECOND_SIG}\",\"keyid\":\"{KEYID}\"}},\
             {{\"sig\":\"{SECOND_SIG}\",\"keyid\":\"SHA256:future\"}}"
        ),
    );
    let (name, tarball) = want("linux-x64");
    let want = expected(&name, &tarball, "linux", "x64");
    assert!(verify_dist(&added, &want, NPM_REGISTRY_KEYS, NOW).is_ok());
    let mut der = aterm_codec::base64::decode_strict(FIRST_SIG.as_bytes()).unwrap();
    *der.last_mut().unwrap() ^= 1;
    let bad = aterm_codec::base64::encode(&der).unwrap();
    let both_bad = edit(
        &edit(&added, FIRST_SIG, &bad),
        &format!("{{\"sig\":\"{SECOND_SIG}\",\"keyid\":\"{KEYID}\"}},"),
        "",
    );
    assert!(
        verify_dist(&both_bad, &want, NPM_REGISTRY_KEYS, NOW)
            .unwrap_err()
            .contains("does not verify")
    );
}

/// The tarball's own package.json is held to the same package, version and platform.
#[test]
fn the_tarballs_package_json_is_held_to_the_same_package() {
    let real = br#"{
  "name": "@xai-official/grok-linux-x64",
  "version": "1.0.70",
  "description": "linux-x64 binary for @xai-official/grok. Do not install directly; install @xai-official/grok instead.",
  "license": "Apache-2.0",
  "files": [
    "bin/",
    "THIRD_PARTY_NOTICES.md"
  ],
  "os": [
    "linux"
  ],
  "cpu": [
    "x64"
  ],
  "publishConfig": {
    "access": "public"
  }
}
"#;
    let (name, tarball) = want("linux-x64");
    let want = expected(&name, &tarball, "linux", "x64");
    check_package_json(real, &want).unwrap();
    for (from, to, why) in [
        (
            "\"version\": \"1.0.70\"",
            "\"version\": \"1.0.71\"",
            "is version",
        ),
        (
            "grok-linux-x64\",",
            "grok-linux-arm64\",",
            "names the package",
        ),
        ("\"x64\"\n", "\"arm64\"\n", "cpu x64"),
        ("\"linux\"\n", "\"linux\", \"win32\"\n", "os linux"),
        (
            "\"license\"",
            "\"name\": \"x\", \"license\"",
            "not strict JSON",
        ),
    ] {
        let got = check_package_json(&edit(real, from, to), &want).unwrap_err();
        assert!(got.contains(why), "want {why:?}, got {got}");
    }
}
