// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Primary keys of the lab model (docs/DESIGN-alab-lab-model-2026-11-01.md §5). Each fits the
//! key grammar (`[A-Za-z0-9_-]{1,26}`, no dot) so a cell key `<pk>.<col>[.<elem>]` stays within
//! the 64-byte Commons name. A digest key names its row everywhere without a lookup; a key
//! whose natural identity is a low-entropy secret (a phone number) or an admins-only value (a
//! mailbox login) is random instead, because an unkeyed hash of it would let any member test
//! guesses against the member-readable key.
//!
//! **The canonical account key** is the one key the importer, the harness, the claims rows and
//! the journal all derive: the claims ring's realm and email (`harness/claims.rs` `LeaseKey`,
//! keyed by `expected_identity_email`: the identity address when set, else the login), so an
//! alias login always joins its claims.

use super::pk_grammar;
use crate::{Error, Result};
use aterm_codec::hex;
use aterm_digest::Sha256;

/// Anthropic's provider word.
pub const ANTHROPIC: &str = "anthropic";
/// OpenAI's provider word.
pub const OPENAI: &str = "openai";
/// Google's provider word.
pub const GOOGLE: &str = "google";
/// Meta's provider word.
pub const META: &str = "meta";
/// Moonshot's Kimi provider word, as the account sheet's column names it.
pub const KIMI: &str = "kimi";
/// xAI's provider word.
pub const XAI: &str = "xai";
/// One provider word per provider, shared by the importer (`import.rs` `PROVIDERS`), the
/// registry and the account key.
pub const PROVIDERS: [&str; 6] = [ANTHROPIC, OPENAI, GOOGLE, META, KIMI, XAI];

/// The canonical realm for `raw`: the claims ring's spelling for a pooled provider (it agrees
/// with `harness/claims.rs` `canonical_pool_realm` on every spelling that accepts, the empty
/// default included), or the provider word for an unpooled one.
pub fn canonical_realm(raw: &str) -> Option<&'static str> {
    match raw.trim_ascii().to_ascii_lowercase().as_str() {
        "" | "claude" | "anthropic" => Some("claude"),
        "codex" | "openai" => Some("codex"),
        "grok" | "x" | "xai" => Some("grok"),
        "kimi" | "kimi:global" | "kimi-global" => Some("kimi:global"),
        "kimi:mainland-cn" | "kimi-mainland-cn" | "kimi:cn" | "kimi-cn" => Some("kimi:mainland-cn"),
        "google" | "gemini" => Some("google"),
        "meta" => Some("meta"),
        _ => None,
    }
}

/// The provider word of a canonical realm.
pub fn provider_of_realm(realm: &str) -> Option<&'static str> {
    match canonical_realm(realm)? {
        "claude" => Some(ANTHROPIC),
        "codex" => Some(OPENAI),
        "grok" => Some(XAI),
        "kimi:global" | "kimi:mainland-cn" => Some(KIMI),
        "google" => Some(GOOGLE),
        "meta" => Some(META),
        _ => None,
    }
}

/// An address lower-cased and ASCII-trimmed, the claims spelling, once it looks like one:
/// one `@` between a non-empty local part and a dotted domain, no space or control character,
/// at most 254 bytes. The refusal never quotes it.
///
/// # Errors
/// It does not look like an address.
pub fn address(raw: &str) -> Result<String> {
    let trimmed = raw.trim_ascii().to_ascii_lowercase();
    let well_formed = trimmed.len() <= 254
        && !trimmed.chars().any(|c| c.is_whitespace() || c.is_control())
        && trimmed.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty()
                && !domain.contains('@')
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
        });
    if well_formed {
        Ok(trimmed)
    } else {
        Err(Error::new("not an address"))
    }
}

/// The address the claims ring keys an account by: its identity when set, else its login
/// (`accounts.rs` `expected_identity_email`).
///
/// # Errors
/// Neither is given, or the chosen one is not an address.
pub fn claims_email(identity: Option<&str>, login: Option<&str>) -> Result<String> {
    address(
        identity
            .or(login)
            .ok_or_else(|| Error::new("an account needs a login address"))?,
    )
}

fn h8(prefix: &str, domain: &[u8], parts: &[&[u8]]) -> String {
    let mut input = domain.to_vec();
    for (n, part) in parts.iter().enumerate() {
        if n > 0 {
            input.push(0);
        }
        input.extend_from_slice(part);
    }
    format!(
        "{prefix}-{}",
        hex::encode_lower(&Sha256::digest(&input)[..8])
    )
}

/// A machine's key from its birth identity: its Commons machine uuid (32 hex) when it
/// registered itself, `m<N>` for a sheet placeholder, or `n-<fleet node>` for a Mac known only
/// from the fleet book. Minted once and never recomputed: a relabel keeps it.
pub fn machine_pk(birth: &str) -> String {
    h8("mc", b"aterm/alab/machine/1\0", &[birth.as_bytes()])
}

/// An address's key: its digest, so the address itself is never a key.
///
/// # Errors
/// `raw` is not an address.
pub fn address_pk(raw: &str) -> Result<String> {
    Ok(h8(
        "ad",
        b"aterm/alab/address/1\0",
        &[address(raw)?.as_bytes()],
    ))
}

/// **The canonical account key**: the canonical realm and the claims email
/// ([`claims_email`]), so the importer, the harness, the claims rows and journal events name
/// one row without a lookup.
///
/// # Errors
/// An unknown realm or a malformed address.
pub fn account_pk(realm: &str, claims_email: &str) -> Result<String> {
    let realm = canonical_realm(realm).ok_or_else(|| Error::new("unknown provider realm"))?;
    Ok(h8(
        "ac",
        b"aterm/alab/account/1\0",
        &[realm.as_bytes(), address(claims_email)?.as_bytes()],
    ))
}

/// A sub-account's key: a provider workspace under one account, by the provider's own
/// `AccountKey` as 12 lower-case hex digits (`profile.rs`).
///
/// # Errors
/// `account` is not an account key or `account_key` is not 12 hex digits.
pub fn subaccount_pk(account: &str, account_key: &str) -> Result<String> {
    if !is_digest_pk(account, "ac") || !is_hex(account_key, 12) {
        return Err(Error::new(
            "a sub-account names an account and a 12-digit key",
        ));
    }
    Ok(h8(
        "sa",
        b"aterm/alab/subaccount/1\0",
        &[account.as_bytes(), account_key.as_bytes()],
    ))
}

/// Prefixes of keys minted at random.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Random {
    /// A mailbox: its login is admins-only, so no digest of it may be member-readable.
    Mailbox,
    /// A phone: its number is a low-entropy secret.
    Phone,
}

/// A fresh random key, minted once at the row's birth.
///
/// # Errors
/// The system random source failed.
pub fn random_pk(kind: Random) -> Result<String> {
    let prefix = match kind {
        Random::Mailbox => "mb",
        Random::Phone => "ph",
    };
    Ok(format!(
        "{prefix}-{}",
        hex::encode_lower(&crate::crypto::random::<8>()?)
    ))
}

/// Sign-in methods and their codes of at most six characters, so an auth-route key fits.
pub const METHOD_CODES: [(&str, &str); 9] = [
    ("email-code", "ecode"),
    ("magic-link", "link"),
    ("password", "pass"),
    ("totp", "totp"),
    ("google-sso", "gsso"),
    ("apple-sso", "asso"),
    ("sms", "sms"),
    ("device-code", "dcode"),
    ("passkey", "pkey"),
];

/// An auth route's key: its account and the method's short code.
///
/// # Errors
/// `account` is not an account key or `method` is unknown.
pub fn auth_route_pk(account: &str, method: &str) -> Result<String> {
    let code = METHOD_CODES
        .iter()
        .find(|(word, _)| *word == method)
        .map(|(_, code)| *code)
        .ok_or_else(|| Error::new("unknown sign-in method"))?;
    let hex = account
        .strip_prefix("ac-")
        .filter(|_| is_digest_pk(account, "ac"))
        .ok_or_else(|| Error::new("an auth route names an account"))?;
    Ok(format!("ar-{hex}-{code}"))
}

/// A seat's key: slot `slot` (1 to 99) on a machine. Machine and slot are the natural key, so
/// two addresses placed in one slot at once are a visible conflict, not two rows.
///
/// # Errors
/// `machine` is not a machine key or `slot` is outside 1 to 99.
pub fn seat_pk(machine: &str, slot: u8) -> Result<String> {
    let hex = machine
        .strip_prefix("mc-")
        .filter(|_| is_digest_pk(machine, "mc"))
        .ok_or_else(|| Error::new("a seat names a machine"))?;
    if !(1..=99).contains(&slot) {
        return Err(Error::new("a seat's slot is 1 to 99"));
    }
    Ok(format!("st-{hex}-{slot}"))
}

/// An import source's key: a digest of its bytes, so a re-run of the same file collapses.
pub fn source_pk(bytes: &[u8]) -> String {
    format!("sr-{}", hex::encode_lower(&Sha256::digest(bytes)[..8]))
}

/// A Mac model's key: `hw.model` with `,` replaced by `-` (`Mac16,8` → `Mac16-8`).
///
/// # Errors
/// The result breaks the key grammar.
pub fn model_pk(hw_model: &str) -> Result<String> {
    let pk = hw_model.trim_ascii().replace(',', "-");
    if pk_grammar(&pk) {
        Ok(pk)
    } else {
        Err(Error::new("a model key breaks the key grammar"))
    }
}

/// Whether `text` is a stored machine label: `m` and a positive number with no leading zero
/// (`m100` and up included), optionally with the membership log's clash suffix `-xxxx` (four
/// lower-case hex digits). Input spellings such as `M109` are folded before they are stored.
/// The one label grammar is `machines::Reading`'s.
pub fn is_label(text: &str) -> bool {
    crate::machines::Label::parse_stored(text).is_ok()
}

fn is_hex(text: &str, digits: usize) -> bool {
    text.len() == digits
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Whether `pk` is `<prefix>-<16 hex>`.
pub fn is_digest_pk(pk: &str, prefix: &str) -> bool {
    pk.strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|hex| is_hex(hex, 16))
}

/// A Commons machine uuid as the 32 lower-case hex digits owned keys carry.
pub fn uuid32(machine: &[u8; 16]) -> String {
    hex::encode_lower(machine)
}

/// The machine uuid a 32-digit key names.
pub fn parse_uuid32(text: &str) -> Option<[u8; 16]> {
    if !is_hex(text, 32) {
        return None;
    }
    crate::database::decode_hex::<16>(text).ok()
}
