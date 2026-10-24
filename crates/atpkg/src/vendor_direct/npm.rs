// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! npm's registry signature over one package version's tarball (design §1.1, §1.5): what
//! vouches for grok's `@xai-official/grok-<plat>` tarball before a byte of it moves.
//!
//! The registry signs `"<name>@<version>:<integrity>"` with ECDSA P-256 (SHA-256, an ASN.1
//! DER signature) under a key it names by the OpenSSH fingerprint of its SSH public-key blob
//! (`SHA256:<base64>`). aterm ships those keys ([`NPM_REGISTRY_KEYS`]); it never reads npm's
//! key endpoint, a dist-tag or the publishing account.

use std::collections::BTreeSet;

use super::digests::AuthenticatedDigest;
use super::version::Version;
use crate::manifest::DigestAlg;

/// One ECDSA P-256 key npm's registry signs package versions with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NpmKey {
    /// How the registry names it: `SHA256:` and the unpadded base64 OpenSSH fingerprint of
    /// [`Self::point`] ([`keyid_of`]), which a signature's keyid must also re-derive to.
    pub keyid: &'static str,
    /// The uncompressed SEC1 point (`0x04 || X || Y`): the registry's SPKI past its 26-byte
    /// P-256 prefix.
    pub point: [u8; 65],
    /// Unix seconds after which a signature under it no longer vouches; `None` while the
    /// registry names no expiry.
    pub not_after: Option<i64>,
}

/// The registry keys a version document's signature is held to: what
/// `https://registry.npmjs.org/-/npm/v1/keys` served on 2026-10-24 (the fixture
/// `npm-registry-keys.json`, whose `note` member carries the export's `gitleaks:allow`
/// tag: these are public keys), less its one key that expired on 2025-01-29. A rotation is a
/// new aterm, never a key read at runtime.
pub(crate) const NPM_REGISTRY_KEYS: &[NpmKey] = &[NpmKey {
    keyid: "SHA256:DhQ8wR5APBvFHLF/+Tc+AYvPOdTpcIDqOhxsBHRwC7U",
    point: [
        0x04, 0x63, 0xa6, 0x1a, 0xed, 0x6f, 0xbe, 0xed, 0xa5, 0x0f, 0xce, 0xf3, 0x13, 0xad, 0xec,
        0xc7, 0xe9, 0x87, 0x31, 0xdd, 0xcf, 0x87, 0x38, 0xa6, 0x02, 0x70, 0xd1, 0xb2, 0x6c, 0x96,
        0x52, 0x08, 0x9a, 0xbf, 0x7d, 0xde, 0xd0, 0x6b, 0xcb, 0xae, 0x00, 0xab, 0x5d, 0x22, 0x45,
        0x10, 0xb5, 0x08, 0x84, 0x28, 0x44, 0x61, 0x02, 0x61, 0x39, 0x94, 0xc3, 0x09, 0x84, 0xff,
        0x0e, 0x90, 0x33, 0x9a, 0xda,
    ],
    not_after: None,
}];

/// The registry's name for the key at `point`: `SHA256:` and the unpadded base64 SHA-256 of
/// its SSH public-key blob (RFC 5656 §3.1: the strings `ecdsa-sha2-nistp256`, `nistp256` and
/// the point, each behind its 32-bit big-endian length).
#[must_use]
pub(crate) fn keyid_of(point: &[u8; 65]) -> String {
    let mut blob = Vec::with_capacity(4 + 19 + 4 + 8 + 4 + 65);
    for part in [&b"ecdsa-sha2-nistp256"[..], b"nistp256", point] {
        blob.extend_from_slice(&u32::try_from(part.len()).unwrap_or(u32::MAX).to_be_bytes());
        blob.extend_from_slice(part);
    }
    let digest = ring::digest::digest(&ring::digest::SHA256, &blob);
    let b64 = aterm_codec::base64::encode(digest.as_ref()).unwrap_or_default();
    format!("SHA256:{}", b64.trim_end_matches('='))
}

/// What one npm package version must be: the package the client builds the name of, for
/// the head's version, served from the tarball URL the client builds, for this platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Expected<'a> {
    /// `@xai-official/grok-linux-x64`.
    pub name: &'a str,
    /// The head's version.
    pub version: Version,
    /// `https://registry.npmjs.org/<name>/-/<file>.tgz`.
    pub tarball: &'a str,
    /// npm's `os` word: `linux`, `darwin`.
    pub os: &'a str,
    /// npm's `cpu` word: `x64`, `arm64`.
    pub cpu: &'a str,
}

/// The tarball sha512 a registry version document vouches for, once it is strict JSON naming
/// exactly `want`'s package, version, tarball, `os` and `cpu`, with one `sha512-` integrity
/// that a key of `keys` unexpired at `now` signed.
///
/// Every signature under a key of `keys` must verify (one that does not refuses the document,
/// expired key or not); a signature under a key aterm does not ship is ignored while one under
/// a shipped key verifies, and refuses the document, naming its key, when none does.
///
/// # Errors
/// One line naming the first rule the document breaks.
pub(crate) fn verify_dist(
    doc: &[u8],
    want: &Expected<'_>,
    keys: &[NpmKey],
    now: i64,
) -> Result<AuthenticatedDigest, String> {
    let at = format!("{}@{}", want.name, want.version);
    let Ok(doc) = super::resolve::strict_json(doc) else {
        return Err(format!(
            "npm's version document for {at} is not strict JSON (a key repeats or it does not \
             parse)"
        ));
    };
    held_to(&doc, "version document", want)?;
    let field = |path: &str| doc.pointer(path).and_then(aterm_json::Value::as_str);
    let tarball = field("/dist/tarball").unwrap_or_default();
    if tarball != want.tarball {
        return Err(format!(
            "npm's version document for {at} names the tarball {tarball:?}, not {}",
            want.tarball
        ));
    }
    let integrity = field("/dist/integrity").unwrap_or_default();
    let Some(sha512) = sha512_of(integrity) else {
        return Err(format!(
            "npm's version document for {at} carries no single `sha512-` integrity of 64 bytes \
             (it names {integrity:?})"
        ));
    };
    let message = format!("{at}:{integrity}");
    signed(&doc, &message, &at, keys, now)?;
    AuthenticatedDigest::authenticated(DigestAlg::Sha512, &sha512)
        .ok_or_else(|| format!("npm's integrity for {at} is not a sha512"))
}

/// Holds an npm package manifest — the version document, or the tarball's own
/// `package/package.json` — to `want`'s name, version, `os` and `cpu`.
///
/// # Errors
/// One line naming the first field that differs.
fn held_to(doc: &aterm_json::Value, what: &str, want: &Expected<'_>) -> Result<(), String> {
    let at = format!("{}@{}", want.name, want.version);
    let field = |key: &str| doc.get(key).and_then(aterm_json::Value::as_str);
    if field("name") != Some(want.name) {
        return Err(format!(
            "npm's {what} names the package {:?}, not {}",
            field("name").unwrap_or_default(),
            want.name
        ));
    }
    if field("version") != Some(want.version.to_string().as_str()) {
        return Err(format!(
            "npm's {what} for {} is version {:?}, not {}",
            want.name,
            field("version").unwrap_or_default(),
            want.version
        ));
    }
    for (key, word) in [("os", want.os), ("cpu", want.cpu)] {
        let words: Option<Vec<&str>> = doc
            .get(key)
            .and_then(|v| match v {
                aterm_json::Value::Array(items) => Some(items),
                _ => None,
            })
            .map(|items| items.iter().filter_map(aterm_json::Value::as_str).collect());
        if words.as_deref() != Some(&[word][..]) {
            return Err(format!(
                "npm's {what} for {at} is not for {key} {word} alone"
            ));
        }
    }
    Ok(())
}

/// Holds the tarball's own `package/package.json` to `want` (the version document already
/// was): strict JSON naming the same package, version, `os` and `cpu`.
///
/// # Errors
/// One line naming the first rule it breaks.
pub(crate) fn check_package_json(bytes: &[u8], want: &Expected<'_>) -> Result<(), String> {
    let Ok(doc) = super::resolve::strict_json(bytes) else {
        return Err(format!(
            "the package.json in npm's tarball of {}@{} is not strict JSON",
            want.name, want.version
        ));
    };
    held_to(&doc, "package.json", want)
}

/// The lowercase-hex sha512 an SRI `integrity` names, when it is exactly one `sha512-` token
/// whose canonical base64 decodes to 64 bytes.
fn sha512_of(integrity: &str) -> Option<String> {
    let b64 = integrity.strip_prefix("sha512-")?;
    let bytes = aterm_codec::base64::decode_strict(b64.as_bytes()).ok()?;
    (bytes.len() == 64).then(|| aterm_codec::hex::encode_lower(&bytes))
}

/// Whether the document's `dist.signatures` vouch for `message` ([`verify_dist`]'s rule).
fn signed(
    doc: &aterm_json::Value,
    message: &str,
    at: &str,
    keys: &[NpmKey],
    now: i64,
) -> Result<(), String> {
    let Some(aterm_json::Value::Array(signatures)) = doc.pointer("/dist/signatures") else {
        return Err(format!(
            "npm's version document for {at} carries no registry signature"
        ));
    };
    let mut vouched = false;
    let mut expired = BTreeSet::new();
    let mut unknown = BTreeSet::new();
    for entry in signatures {
        let keyid = entry.get("keyid").and_then(aterm_json::Value::as_str);
        let sig = entry
            .get("sig")
            .and_then(aterm_json::Value::as_str)
            .and_then(|s| aterm_codec::base64::decode_strict(s.as_bytes()).ok());
        let (Some(keyid), Some(sig)) = (keyid, sig) else {
            return Err(format!(
                "npm's version document for {at} carries a registry signature that is not a \
                 keyid and a base64 signature"
            ));
        };
        let Some(key) = keys
            .iter()
            .find(|k| k.keyid == keyid && keyid_of(&k.point) == keyid)
        else {
            unknown.insert(keyid.to_string());
            continue;
        };
        let verifies = ring::signature::UnparsedPublicKey::new(
            &ring::signature::ECDSA_P256_SHA256_ASN1,
            &key.point[..],
        )
        .verify(message.as_bytes(), &sig)
        .is_ok();
        if !verifies {
            return Err(format!(
                "npm's registry signature under {keyid} does not verify {at}'s integrity"
            ));
        }
        if key.not_after.is_some_and(|end| now > end) {
            expired.insert(keyid.to_string());
        } else {
            vouched = true;
        }
    }
    if vouched {
        return Ok(());
    }
    let named = |set: &BTreeSet<String>| set.iter().cloned().collect::<Vec<_>>().join(", ");
    Err(if !unknown.is_empty() {
        format!(
            "npm's version document for {at} is signed only by registry keys aterm does not \
             ship ({}): a registry key rotation needs a newer aterm",
            named(&unknown)
        )
    } else if !expired.is_empty() {
        format!(
            "npm's version document for {at} is signed only under expired registry keys ({})",
            named(&expired)
        )
    } else {
        format!("npm's version document for {at} carries no registry signature")
    })
}

#[cfg(test)]
mod tests {
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
            0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06,
            0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
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
            let spki =
                aterm_codec::base64::decode_strict(field("key").unwrap().as_bytes()).unwrap();
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
                aterm_codec::base64::decode_strict(&INTEGRITY.as_bytes()["sha512-".len()..])
                    .unwrap();
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
                edit(LINUX_X64, "\"cpu\":[\"x64\"]", "\"cpu\":[\"arm64\"]"),
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
}
