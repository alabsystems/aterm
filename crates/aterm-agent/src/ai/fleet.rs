// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FLEET DOCUMENT FROM A URL (docs/DESIGN-ai-connections-2026-10-16.md §6.3).
//!
//! The owner keeps one fleet document for every machine, signs it with a key only they hold
//! (`aterm ai fleet keygen`, `aterm ai fleet sign`), and serves it and its `.sig` side by side
//! from any HTTPS address. Each machine names the address in `[ai] fleet` and PINS the
//! signer's public key in `[ai] fleet_key`, so the address alone can never change what a
//! machine trusts:
//!
//! * fetched over HTTPS only, size-capped (`aterm_update_core::download_bytes`), at most
//!   every [`FETCH_EVERY_S`] unless a person asks;
//! * verified (Ed25519 over the exact bytes, the updater's own verifier) before it is parsed;
//! * carrying a `seq` that never goes down (a server cannot hand back an older document) and,
//!   optionally, a `valid_until` past which it is not used;
//! * kept, with its signature, at `<state>/ai/fleet/`, and verified again at every read, so
//!   the machine keeps the last good document while offline and a tampered copy is refused.
//!
//! A document read from a local path is verified the same way when `[ai] fleet_key` is set.

use std::path::{Path, PathBuf};

use aterm_json::{Map, Number, Value};

use super::config::FLEET_MAX_BYTES;

/// How often a machine fetches the document on its own.
pub const FETCH_EVERY_S: i64 = 6 * 3600;
/// The largest signature file read.
pub const SIG_MAX: u64 = 1024;
/// The cache's directory under the AI store.
pub const CACHE_DIR: &str = "fleet";
/// The cached document.
pub const DOC_FILE: &str = "fleet.toml";
/// Its signature.
pub const SIG_FILE: &str = "fleet.toml.sig";
/// What the cache knows of its source.
pub const STATE_FILE: &str = "state.json";

/// Whether `[ai] fleet` names a URL rather than a file.
#[must_use]
pub fn is_url(source: &str) -> bool {
    source.starts_with("https://")
}

/// The raw 32-byte Ed25519 public key `b64` names, if it names one.
#[must_use]
pub fn key_bytes(b64: &str) -> Option<Vec<u8>> {
    aterm_codec::base64::decode_strict(b64.trim().as_bytes())
        .ok()
        .filter(|k| k.len() == 32)
}

/// A key's short fingerprint for display: its first 12 base64 characters.
#[must_use]
pub fn fingerprint(b64: &str) -> String {
    b64.trim().chars().take(12).collect()
}

/// A signature file's 64 bytes: raw, or base64 text (what `aterm ai fleet sign` writes).
#[must_use]
pub fn sig_bytes(raw: &[u8]) -> Option<Vec<u8>> {
    if raw.len() == 64 {
        return Some(raw.to_vec());
    }
    let text = std::str::from_utf8(raw).ok()?.trim();
    aterm_codec::base64::decode_strict(text.as_bytes())
        .ok()
        .filter(|s| s.len() == 64)
}

/// Verifies `doc` under the pinned `key` against `sig` (raw or base64).
///
/// # Errors
///
/// Why it does not verify, in one closed phrase.
pub fn verify(doc: &[u8], sig: &[u8], key: &str) -> Result<(), String> {
    if key_bytes(key).is_none() {
        return Err("[ai] fleet_key is not an Ed25519 public key".to_string());
    }
    let sig = sig_bytes(sig).ok_or("its signature file holds no signature")?;
    aterm_update_core::roster::verify_roster(&[key.trim()], doc.to_vec(), &sig)
        .map(|_| ())
        .map_err(|_| "its signature does not verify under [ai] fleet_key".to_string())
}

/// The document's own `seq` (required for a document from a URL) and `valid_until` (unix
/// seconds or RFC 3339), read from its top level.
///
/// # Errors
///
/// Why they cannot be read.
pub fn meta(text: &str) -> Result<(Option<i64>, Option<i64>), String> {
    let doc =
        aterm_toml::from_str::<aterm_toml::Value>(text).map_err(|_| "not TOML".to_string())?;
    let seq = match doc.get("seq") {
        None => None,
        Some(v) => Some(
            v.as_integer()
                .filter(|n| *n >= 0)
                .ok_or("`seq` takes a whole number, 0 or more")?,
        ),
    };
    let valid_until = match doc.get("valid_until") {
        None => None,
        Some(aterm_toml::Value::Integer(n)) => Some(*n),
        Some(aterm_toml::Value::String(s)) => Some(
            aterm_types::rfc3339::parse_utc(s).ok_or("`valid_until` is not an RFC 3339 time")?,
        ),
        Some(aterm_toml::Value::Datetime(d)) => Some(
            aterm_types::rfc3339::parse_utc(&d.to_string())
                .ok_or("`valid_until` is not an RFC 3339 time")?,
        ),
        Some(_) => return Err("`valid_until` takes a time".to_string()),
    };
    Ok((seq, valid_until))
}

/// What the cache knows of its source.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Held {
    /// The URL it came from.
    pub url: String,
    /// The key it verified under.
    pub key: String,
    /// The held document's `seq`.
    pub seq: Option<i64>,
    /// When it was fetched, unix seconds.
    pub fetched_at: Option<i64>,
    /// When a fetch was last tried.
    pub tried_at: Option<i64>,
    /// The held document's `valid_until`.
    pub valid_until: Option<i64>,
    /// Why the last try failed, if it did.
    pub error: Option<String>,
}

impl Held {
    fn to_json(&self) -> String {
        let mut o = Map::new();
        let int = |n: i64| Value::Number(Number::from(n));
        o.insert("schema".to_string(), int(1));
        o.insert("url".to_string(), Value::from(self.url.as_str()));
        o.insert("key".to_string(), Value::from(self.key.as_str()));
        for (k, v) in [
            ("seq", self.seq),
            ("fetched_at", self.fetched_at),
            ("tried_at", self.tried_at),
            ("valid_until", self.valid_until),
        ] {
            if let Some(v) = v {
                o.insert(k.to_string(), int(v));
            }
        }
        if let Some(e) = &self.error {
            o.insert("error".to_string(), Value::from(e.as_str()));
        }
        aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
    }

    fn of_json(text: &str) -> Option<Held> {
        let v: Value = aterm_json::from_str(text).ok()?;
        (v.get("schema")?.as_i64()? == 1).then_some(())?;
        let int = |k: &str| v.get(k).and_then(Value::as_i64);
        Some(Held {
            url: v.get("url")?.as_str()?.to_string(),
            key: v.get("key")?.as_str()?.to_string(),
            seq: int("seq"),
            fetched_at: int("fetched_at"),
            tried_at: int("tried_at"),
            valid_until: int("valid_until"),
            error: v
                .get("error")
                .and_then(Value::as_str)
                .map(|s| crate::harness::one_line(s, 160)),
        })
    }

    /// Whether this record is of `url` under `key`.
    #[must_use]
    pub fn of(&self, url: &str, key: &str) -> bool {
        self.url == url && self.key.trim() == key.trim()
    }
}

/// The cache's directory under the AI store's directory.
#[must_use]
pub fn cache_dir(ai_dir: &Path) -> PathBuf {
    ai_dir.join(CACHE_DIR)
}

/// What the cache records, if anything.
#[must_use]
pub fn held_state(ai_dir: &Path) -> Option<Held> {
    crate::harness::accounts::read_bounded(&cache_dir(ai_dir).join(STATE_FILE), 16 * 1024)
        .ok()
        .flatten()
        .and_then(|t| Held::of_json(&t))
}

/// The held document of `url` under `key`, verified again and within its `valid_until`.
///
/// # Errors
///
/// Why there is none to use: not fetched yet, tampered with, or expired.
pub fn held(ai_dir: &Path, url: &str, key: &str, now: i64) -> Result<String, String> {
    let state = held_state(ai_dir)
        .filter(|h| h.of(url, key) && h.fetched_at.is_some())
        .ok_or("not fetched yet")?;
    let dir = cache_dir(ai_dir);
    let doc = crate::harness::accounts::read_bounded(&dir.join(DOC_FILE), FLEET_MAX_BYTES)
        .ok()
        .flatten()
        .ok_or("its held copy is gone")?;
    let sig = std::fs::read(dir.join(SIG_FILE)).map_err(|_| "its held signature is gone")?;
    verify(doc.as_bytes(), &sig, key).map_err(|why| format!("its held copy: {why}"))?;
    // The expiry is the signed document's own, not the record's.
    let (_, valid_until) = meta(&doc).map_err(|why| format!("its held copy: {why}"))?;
    let _ = state;
    if let Some(until) = valid_until.filter(|t| *t < now) {
        return Err(format!(
            "its held copy expired at {}",
            super::state::when(until)
        ));
    }
    Ok(doc)
}

/// What one fetch came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetched {
    /// A newer document was verified and is now held.
    Fresh(i64),
    /// The served document is the one held.
    Same(i64),
    /// A fetch was made within [`FETCH_EVERY_S`]; nothing was asked.
    NotDue,
    /// The served document was refused, and the held one stands; why.
    Refused(String),
    /// Nothing could be fetched, and the held one stands; why.
    Unreachable(String),
}

impl Fetched {
    /// In a person's words.
    #[must_use]
    pub fn says(&self) -> String {
        match self {
            Fetched::Fresh(seq) => format!("fetched and verified seq {seq}"),
            Fetched::Same(seq) => format!("unchanged (seq {seq})"),
            Fetched::NotDue => "fetched recently; not asked again".to_string(),
            Fetched::Refused(why) => format!("refused: {why}"),
            Fetched::Unreachable(why) => format!("could not be fetched: {why}"),
        }
    }
}

/// Gets a URL's bytes, capped. Injected: tests reach no network.
pub trait Fetcher {
    /// The body of `url`, at most `max` bytes.
    ///
    /// # Errors
    ///
    /// Why it could not be fetched.
    fn get(&self, url: &str, max: u64) -> Result<Vec<u8>, String>;
}

/// The shipped [`Fetcher`]: HTTPS only, size-capped, retried (the updater's own download).
#[derive(Debug, Clone, Copy, Default)]
pub struct LiveFetcher;

impl Fetcher for LiveFetcher {
    fn get(&self, url: &str, max: u64) -> Result<Vec<u8>, String> {
        aterm_update_core::download_bytes(url, max)
    }
}

fn write_private(dir: &Path, name: &str, text: &str) -> std::io::Result<()> {
    aterm_types::fs_restricted::ensure_private_dir(dir)?;
    crate::harness::upgrade_models::write_atomic(&dir.join(name), text)
}

/// Fetches the document at `url` and its `<url>.sig`, verifies it under `key`, and holds it
/// when it is newer than (or as new as) the one held. `force` asks even within
/// [`FETCH_EVERY_S`] of the last try. A failure leaves the held document as it was.
pub fn fetch(
    ai_dir: &Path,
    url: &str,
    key: &str,
    now: i64,
    fetcher: &dyn Fetcher,
    force: bool,
) -> Fetched {
    if !is_url(url) {
        return Fetched::Refused("`[ai] fleet` is not an https:// URL".to_string());
    }
    if key_bytes(key).is_none() {
        return Fetched::Refused(
            "no [ai] fleet_key pins the document's signer (aterm ai fleet keygen makes one)"
                .to_string(),
        );
    }
    let mut state = held_state(ai_dir)
        .filter(|h| h.of(url, key))
        .unwrap_or_else(|| Held {
            url: url.to_string(),
            key: key.trim().to_string(),
            ..Held::default()
        });
    if !force
        && state
            .tried_at
            .is_some_and(|t| now.saturating_sub(t) < FETCH_EVERY_S)
    {
        return Fetched::NotDue;
    }
    let dir = cache_dir(ai_dir);
    let mut record = |state: &mut Held, error: Option<String>| {
        state.tried_at = Some(now);
        state.error = error;
        let _ = write_private(&dir, STATE_FILE, &state.to_json());
    };
    let got = fetcher.get(url, FLEET_MAX_BYTES).and_then(|doc| {
        fetcher
            .get(&format!("{url}.sig"), SIG_MAX)
            .map(|sig| (doc, sig))
    });
    let (doc, sig) = match got {
        Ok(pair) => pair,
        Err(why) => {
            let why = crate::harness::one_line(&why, 160);
            record(&mut state, Some(why.clone()));
            return Fetched::Unreachable(why);
        }
    };
    let refuse =
        |state: &mut Held, why: String, record: &mut dyn FnMut(&mut Held, Option<String>)| {
            record(state, Some(why.clone()));
            Fetched::Refused(why)
        };
    if let Err(why) = verify(&doc, &sig, key) {
        return refuse(&mut state, why, &mut record);
    }
    let Ok(text) = String::from_utf8(doc) else {
        return refuse(&mut state, "it is not UTF-8 text".to_string(), &mut record);
    };
    if let Err(why) = super::config::read_fleet(&text) {
        return refuse(&mut state, why, &mut record);
    }
    let (seq, valid_until) = match meta(&text) {
        Ok((Some(seq), valid_until)) => (seq, valid_until),
        Ok((None, _)) => {
            return refuse(
                &mut state,
                "it has no `seq` (a document from a URL needs one)".to_string(),
                &mut record,
            );
        }
        Err(why) => return refuse(&mut state, why, &mut record),
    };
    if let Some(until) = valid_until.filter(|t| *t < now) {
        return refuse(
            &mut state,
            format!("it expired at {}", super::state::when(until)),
            &mut record,
        );
    }
    if let Some(held_seq) = state.seq.filter(|h| *h > seq) {
        return refuse(
            &mut state,
            format!("it is older than the one held (seq {seq} < {held_seq})"),
            &mut record,
        );
    }
    let same = state.seq == Some(seq);
    // The signature is held as base64 text, whichever form it was served in.
    let sig_text = sig_bytes(&sig)
        .and_then(|b| aterm_codec::base64::encode(&b).ok())
        .unwrap_or_default();
    let written = write_private(&dir, DOC_FILE, &text)
        .and_then(|()| write_private(&dir, SIG_FILE, &sig_text));
    if let Err(e) = written {
        let why = format!("it could not be kept: {e}");
        record(&mut state, Some(why.clone()));
        return Fetched::Unreachable(why);
    }
    state.seq = Some(seq);
    state.valid_until = valid_until;
    state.fetched_at = Some(now);
    record(&mut state, None);
    if same {
        Fetched::Same(seq)
    } else {
        Fetched::Fresh(seq)
    }
}

/// A new signing key: the secret key's PKCS#8 bytes and the public key in base64.
///
/// # Errors
///
/// Why no key could be made.
pub fn keygen() -> Result<(Vec<u8>, String), String> {
    use ring::signature::KeyPair as _;
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng)
        .map_err(|_| "no key could be made".to_string())?;
    let pair = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
        .map_err(|_| "the new key does not read back".to_string())?;
    let public = aterm_codec::base64::encode(pair.public_key().as_ref())
        .map_err(|_| "the public key does not encode".to_string())?;
    Ok((pkcs8.as_ref().to_vec(), public))
}

/// The public key of a secret key file's bytes, in base64.
///
/// # Errors
///
/// Why the bytes are no secret key.
pub fn public_of(secret: &[u8]) -> Result<String, String> {
    use ring::signature::KeyPair as _;
    let pair = ring::signature::Ed25519KeyPair::from_pkcs8(secret)
        .map_err(|_| "not an Ed25519 secret key (PKCS#8)".to_string())?;
    aterm_codec::base64::encode(pair.public_key().as_ref())
        .map_err(|_| "the public key does not encode".to_string())
}

/// The signature of `doc` under the secret key, as base64 text.
///
/// # Errors
///
/// Why the bytes are no secret key.
pub fn sign(secret: &[u8], doc: &[u8]) -> Result<String, String> {
    let pair = ring::signature::Ed25519KeyPair::from_pkcs8(secret)
        .map_err(|_| "not an Ed25519 secret key (PKCS#8)".to_string())?;
    aterm_codec::base64::encode(pair.sign(doc).as_ref())
        .map_err(|_| "the signature does not encode".to_string())
}

#[cfg(test)]
#[path = "fleet_tests.rs"]
mod tests;
