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
use aterm_digest::{Sha256, hmac_sha256_parts};

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
    let trimmed = raw.trim_ascii();
    if trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("claude")
        || trimmed.eq_ignore_ascii_case("anthropic")
    {
        Some("claude")
    } else if trimmed.eq_ignore_ascii_case("codex") || trimmed.eq_ignore_ascii_case("openai") {
        Some("codex")
    } else if trimmed.eq_ignore_ascii_case("grok")
        || trimmed.eq_ignore_ascii_case("x")
        || trimmed.eq_ignore_ascii_case("xai")
    {
        Some("grok")
    } else if trimmed.eq_ignore_ascii_case("kimi")
        || trimmed.eq_ignore_ascii_case("kimi:global")
        || trimmed.eq_ignore_ascii_case("kimi-global")
    {
        Some("kimi:global")
    } else if trimmed.eq_ignore_ascii_case("kimi:mainland-cn")
        || trimmed.eq_ignore_ascii_case("kimi-mainland-cn")
        || trimmed.eq_ignore_ascii_case("kimi:cn")
        || trimmed.eq_ignore_ascii_case("kimi-cn")
    {
        Some("kimi:mainland-cn")
    } else if trimmed.eq_ignore_ascii_case("google") || trimmed.eq_ignore_ascii_case("gemini") {
        Some("google")
    } else if trimmed.eq_ignore_ascii_case("meta") {
        Some("meta")
    } else {
        None
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

fn is_well_formed_address(trimmed: &str) -> bool {
    trimmed.len() <= 254
        && !trimmed.chars().any(|c| c.is_whitespace() || c.is_control())
        && trimmed.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty()
                && !domain.contains('@')
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
        })
}

pub(crate) fn is_stored_address(raw: &str) -> bool {
    raw.trim_ascii().len() == raw.len()
        && !raw.bytes().any(|b| b.is_ascii_uppercase())
        && is_well_formed_address(raw)
}

fn address_cow(raw: &str) -> Result<std::borrow::Cow<'_, str>> {
    let trimmed = raw.trim_ascii();
    if !is_well_formed_address(trimmed) {
        return Err(Error::new("not an address"));
    }
    if trimmed.bytes().any(|b| b.is_ascii_uppercase()) {
        Ok(std::borrow::Cow::Owned(trimmed.to_ascii_lowercase()))
    } else {
        Ok(std::borrow::Cow::Borrowed(trimmed))
    }
}

/// An address lower-cased and ASCII-trimmed, the claims spelling, once it looks like one:
/// one `@` between a non-empty local part and a dotted domain, no space or control character,
/// at most 254 bytes. The refusal never quotes it.
///
/// # Errors
/// It does not look like an address.
pub fn address(raw: &str) -> Result<String> {
    address_cow(raw).map(std::borrow::Cow::into_owned)
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

fn prefix_hex(prefix: &str, bytes: &[u8]) -> String {
    let mut out = String::with_capacity(prefix.len() + 1 + bytes.len() * 2);
    out.push_str(prefix);
    out.push('-');
    hex::push_lower(&mut out, bytes);
    out
}

fn h8(prefix: &str, domain: &[u8], parts: &[&[u8]]) -> String {
    let mut sha = Sha256::new();
    sha.update(domain);
    for (n, part) in parts.iter().enumerate() {
        if n > 0 {
            sha.update([0]);
        }
        sha.update(part);
    }
    prefix_hex(prefix, &sha.finalize()[..8])
}

/// A machine's key from its birth identity: its Commons machine uuid (32 hex) when it
/// registered itself, `m<N>` for a sheet placeholder, or `n-<fleet node>` for a Mac known only
/// from the fleet book. Minted once and never recomputed: a relabel keeps it.
pub fn machine_pk(birth: &str) -> String {
    h8("mc", b"aterm/alab/machine/1\0", &[birth.as_bytes()])
}

/// A planned machine's key for `label`: its birth key [`machine_pk`]`(label)`, or, where a row
/// already holds that (`held`: a retired Mac, or one relabelled away from its birth label), the
/// first `machine_pk("<label>/<n>")` from `n = 2` no row holds, so a replacement Mac gets a row
/// of its own and the old row keeps its history. Deterministic, so two writers planning one
/// label at once make one row. `None` only when every key is held.
pub fn planned_machine_pk(label: &str, held: impl Fn(&str) -> bool) -> Option<String> {
    std::iter::once(machine_pk(label))
        .chain((2u32..).map(|n| machine_pk(&format!("{label}/{n}"))))
        .find(|pk| !held(pk))
}

/// A Mac's hardware hint under one network, 16 hex (design §4.7): `HMAC(key, "aterm/alab/hw-hint/1"
/// ‖ IOPlatformUUID)[..8]`, the key a digest of the network identity, not an epoch key, so it stays
/// stable across rotations and says nothing of the hardware outside this network. The uuid is read
/// in one spelling, so two readings of one Mac give one hint.
pub fn hw_hint(network: &[u8; 16], platform_uuid: &str) -> String {
    let mut sha = Sha256::new();
    sha.update(b"aterm/alab/hw-hint-key/1\0");
    sha.update(network);
    let trimmed = platform_uuid.trim();
    let upper;
    let uuid_bytes = if trimmed.bytes().any(|b| b.is_ascii_lowercase()) {
        upper = trimmed.to_ascii_uppercase();
        upper.as_bytes()
    } else {
        trimmed.as_bytes()
    };
    let tag = hmac_sha256_parts(&sha.finalize(), &[b"aterm/alab/hw-hint/1", uuid_bytes]);
    hex::encode_lower(&tag[..8])
}

/// An address's key: its digest, so the address itself is never a key.
///
/// # Errors
/// `raw` is not an address.
pub fn address_pk(raw: &str) -> Result<String> {
    let addr = address_cow(raw)?;
    Ok(h8("ad", b"aterm/alab/address/1\0", &[addr.as_bytes()]))
}

/// **The canonical account key**: the canonical realm and the claims email
/// ([`claims_email`]), so the importer, the harness, the claims rows and journal events name
/// one row without a lookup.
///
/// # Errors
/// An unknown realm or a malformed address.
pub fn account_pk(realm: &str, claims_email: &str) -> Result<String> {
    let realm = canonical_realm(realm).ok_or_else(|| Error::new("unknown provider realm"))?;
    let email = address_cow(claims_email)?;
    Ok(h8(
        "ac",
        b"aterm/alab/account/1\0",
        &[realm.as_bytes(), email.as_bytes()],
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

/// A mailbox's key from the key of the address it signs in as (`mailbox.login`): `mb-` and 8
/// bytes of SHA-256 over a domain and the `ad-` key, so every Mac that knows the login names
/// the same row with no key file.
///
/// # Errors
/// `login` is not an address key.
pub fn mailbox_pk(login: &str) -> Result<String> {
    if !is_digest_pk(login, "ad") {
        return Err(Error::new("a mailbox's key derives from an address key"));
    }
    Ok(h8("mb", b"aterm/alab/mailbox/2\0", &[login.as_bytes()]))
}

/// Prefixes of keys minted at random.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Random {
    /// A phone: its number is a low-entropy secret, found by its sealed `number` cell.
    Phone,
}

/// A fresh random key, minted once at the row's birth.
///
/// # Errors
/// The system random source failed.
pub fn random_pk(kind: Random) -> Result<String> {
    let prefix = match kind {
        Random::Phone => "ph",
    };
    Ok(prefix_hex(prefix, &crate::crypto::random::<8>()?))
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

/// A preference's key: one machine prefers one account. Only the pair is in the key, so the
/// row's rank, its note and whether it has ended all merge per cell on one row, and no place on
/// a Mac is an identity (design §0).
///
/// # Errors
/// `machine` is not a machine key or `account` is not an account key.
pub fn preference_pk(machine: &str, account: &str) -> Result<String> {
    if !is_digest_pk(machine, "mc") || !is_digest_pk(account, "ac") {
        return Err(Error::new("a preference names a machine and an account"));
    }
    let mut sha = Sha256::new();
    sha.update(b"aterm/alab/preference/1\0");
    sha.update(machine.as_bytes());
    sha.update([0]);
    sha.update(account.as_bytes());
    Ok(prefix_hex("pf", &sha.finalize()[..11]))
}

/// The CLI word a realm's default account label starts with: the CLI that signs in to it.
fn realm_cli(realm: &str) -> Option<&'static str> {
    Some(match canonical_realm(realm)? {
        "claude" => "claude",
        "codex" => "codex",
        "grok" => "grok",
        "kimi:global" | "kimi:mainland-cn" => "kimi",
        "google" => "agy",
        "meta" => "muse",
        _ => return None,
    })
}

/// An account's default label, `<cli>-<6 hex of its key>`: it names no machine and no address,
/// so a row keeps it wherever the account runs.
///
/// # Errors
/// An unknown realm, or `account` is not an account key.
pub fn default_account_label(realm: &str, account: &str) -> Result<String> {
    let cli = realm_cli(realm).ok_or_else(|| Error::new("unknown provider realm"))?;
    let hex = account
        .strip_prefix("ac-")
        .filter(|_| is_digest_pk(account, "ac"))
        .ok_or_else(|| Error::new("a label names an account"))?;
    Ok(format!("{cli}-{}", &hex[..6]))
}

/// An import source's key: a digest of its bytes, so a re-run of the same file collapses.
pub fn source_pk(bytes: &[u8]) -> String {
    prefix_hex("sr", &Sha256::digest(bytes)[..8])
}

/// An archived owned record's key in a later table generation (design §3.8): a digest of the
/// operation it carries, so every Mac that carries one record names one row.
pub fn archived_pk(operation: &[u8; 32]) -> String {
    h8("ar", b"aterm/alab/archived/1\0", &[operation])
}

/// A table roll's cover row (design §3.8): one per generation rolled from and Mac that restated
/// it, so a Mac's later restatement of what it still held replaces its own earlier cover.
pub fn roll_pk(from: u32, uuid32: &str) -> String {
    h8(
        "rl",
        b"aterm/alab/roll/1\0",
        &[&from.to_le_bytes(), uuid32.as_bytes()],
    )
}

/// A writer of a rolled table as its cover row names it: 32 hex digits of a digest of its
/// Commons actor, which is 48 bytes and would not fit a cover's record at 128 Macs.
pub fn actor_key(actor: &[u8]) -> String {
    let mut sha = Sha256::new();
    sha.update(b"aterm/alab/actor/1\0");
    sha.update(actor);
    hex::encode_lower(&sha.finalize()[..16])
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
