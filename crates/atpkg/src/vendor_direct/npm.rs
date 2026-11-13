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
            .and_then(|items| items.iter().map(aterm_json::Value::as_str).collect());
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
        #[expect(
            clippy::disallowed_methods,
            reason = "npm signs with ECDSA P-256, which astream's Ed25519 verify does not cover"
        )]
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
#[path = "npm_tests.rs"]
mod tests;
