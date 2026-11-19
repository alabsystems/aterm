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
use astream_wire::sync::Digest;
use aterm_codec::hex;
use aterm_digest::{Sha256, hmac_sha256_parts};
use std::collections::{BTreeMap, BTreeSet};

/// A key as diagnostics print it, concealing credential-shaped values.
pub(crate) fn safe_key(key: &str) -> &str {
    if super::guard::credential_shaped(key) {
        super::grid::REFUSED
    } else {
        key
    }
}

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

/// Every canonical realm ([`canonical_realm`]): what an account's `realm` cell holds, the one
/// spelling every Mac and every build reads.
pub const REALMS: [&str; 7] = [
    "claude",
    "codex",
    "google",
    "grok",
    "kimi:global",
    "kimi:mainland-cn",
    "meta",
];

/// The canonical realm for `raw`: the claims ring's spelling for a pooled provider (it agrees
/// with `harness/claims.rs` `canonical_pool_realm` on every spelling that accepts, the empty
/// default included), or the provider word for an unpooled one. Each realm's own word as every
/// surface prints it ([`provider_word`]: `oai`, `kimicn`) names it too. agy is the one pooled CLI whose
/// claims realm is not its account realm: its accounts are Google's, so the claims ring's `agy`
/// (and `antigravity`) key as `google`, the realm a read of agy is attributed by and the
/// importer files its rows under.
pub fn canonical_realm(raw: &str) -> Option<&'static str> {
    let trimmed = raw.trim_ascii();
    if trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("claude")
        || trimmed.eq_ignore_ascii_case("anthropic")
    {
        Some("claude")
    } else if trimmed.eq_ignore_ascii_case("codex")
        || trimmed.eq_ignore_ascii_case("openai")
        || trimmed.eq_ignore_ascii_case("oai")
    {
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
        || trimmed.eq_ignore_ascii_case("kimicn")
    {
        Some("kimi:mainland-cn")
    } else if trimmed.eq_ignore_ascii_case("google")
        || trimmed.eq_ignore_ascii_case("gemini")
        || trimmed.eq_ignore_ascii_case("agy")
        || trimmed.eq_ignore_ascii_case("antigravity")
    {
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

pub(crate) fn address_cow(raw: &str) -> Result<std::borrow::Cow<'_, str>> {
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
    /// A phone: found by its `number` cell; a key made from the number would change with it.
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

/// The provider word of an account's realm (docs/DESIGN-shared-files-2026-11-11.md §10): what
/// every surface says before the account's address, and what its slug starts with. A product
/// word, so Kimi's two realms stay apart, and `kimicn` holds no `-`, so no Kimi account's handle
/// spells another realm's slug.
pub fn provider_word(realm: &str) -> Option<&'static str> {
    Some(match canonical_realm(realm)? {
        "claude" => "claude",
        "codex" => "oai",
        "google" => "google",
        "meta" => "meta",
        "grok" => "xai",
        "kimi:global" => "kimi",
        "kimi:mainland-cn" => "kimicn",
        _ => return None,
    })
}

/// An account as it is shown: its provider word and its claims address
/// (`claude ada.fb@alpha.example`). One address signed in to three providers is three names.
pub fn account_name(realm: &str, claims_email: &str) -> Option<String> {
    Some(format!("{} {claims_email}", provider_word(realm)?))
}

/// The most bytes of a slug: one less than the run of 40 letters, digits and `-` the value
/// guards withhold as credential-shaped (`guard::credential_shaped`), so no slug is withheld for
/// its length alone. A slug the guards withhold for its shape names no account
/// ([`account_slugs`]): an `xai-` slug whose handle holds 20 bytes or more reads as an xAI key,
/// so an xAI account's handle holds at most 19. The bound also keeps a slug a vault name inside
/// `claude-code-<label>-oauth`, the entry the vault keeps a roster row's sign-in snapshot under
/// (`aterm_secrets::envelope`'s grammar, `[a-z0-9][a-z0-9-]{0,62}`, leaves 45 bytes there), as
/// it keeps the [`reserved_slug`] a new row is labelled by. aterm-file holds the value, so a
/// reader of the shared files alone checks a slug against the same bound
/// (`aterm_file::account::is_slug`).
pub const MAX_SLUG: usize = aterm_file::account::MAX_SLUG;

/// The most bytes of the word a slug starts with: every provider word ([`provider_word`];
/// `google` and `kimicn` are the longest), and an unknown realm's, which is cut to fit.
pub const MAX_WORD: usize = 6;

/// The most bytes a slug's handle holds: whatever the word, the slug fits [`MAX_SLUG`].
pub const MAX_HANDLE: usize = MAX_SLUG - MAX_WORD - 1;

/// `text` lower-cased, every run of bytes outside `[a-z0-9]` written as one `-`, with none
/// leading or trailing.
fn handle_of(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        let byte = byte.to_ascii_lowercase();
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() {
            out.push(char::from(byte));
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    if out.ends_with('-') {
        out.pop();
    }
    out
}

/// The handles an address derives (design §10): the short one, `<tail>-<site>`, where `tail` is
/// the text after the first `+` of the local part, else after its first `.`, else the whole
/// local part, and `site` is the domain up to its first `.`; and the long one, built the same
/// way from the whole local part, which two accounts of one realm take when their short
/// handles are the same. `None` for text that is no address.
pub fn address_handles(claims_email: &str) -> Option<(String, String)> {
    let (local, domain) = claims_email.trim_ascii().split_once('@')?;
    let site = domain.split('.').next().unwrap_or(domain);
    let tail = local
        .split_once('+')
        .or_else(|| local.split_once('.'))
        .map_or(local, |(_, tail)| tail);
    Some((
        handle_of(&format!("{tail}-{site}")),
        handle_of(&format!("{local}-{site}")),
    ))
}

/// One account, as [`account_slugs`] reads it.
#[derive(Clone, Copy, Debug)]
pub struct Slugged<'a> {
    /// Its key, `ac-<16 hex>`.
    pub pk: &'a str,
    pub realm: &'a str,
    /// The handle its owner named it by: the `label` its row holds, and the operation that
    /// wrote it, a carried candidate's own (`crate::alab::account_names` reads it; the authority
    /// rules hold a label an import or an automatic path wrote inert, so it never reaches here).
    pub handle: Option<(&'a str, Digest)>,
    /// Its claims address, when its address rows resolve.
    pub claims_email: Option<&'a str>,
    pub live: bool,
}

/// `<word>-<handle>`, where the handle is one a slug may hold: not empty, at most
/// [`MAX_HANDLE`] bytes, and the slug not one the value guards withhold (`guard::withheld`).
fn handle_slug(word: &str, handle: &str) -> Option<String> {
    if handle.is_empty() || handle.len() > MAX_HANDLE {
        return None;
    }
    let slug = format!("{word}-{handle}");
    (!super::guard::withheld(&slug)).then_some(slug)
}

/// The slug an owner's `label` makes for an account of `realm` (design §10): `<provider
/// word>-<handle>`, the handle being the label lower-cased with each run of bytes outside
/// `[a-z0-9]` one `-`. `None` where it makes none: the handle is empty or longer than
/// [`MAX_HANDLE`] bytes, or the value guards would withhold the slug (an `xai-` slug whose
/// handle holds 20 bytes or more reads as an xAI key). Such a label names no account, and the
/// registry refuses an owner's write of one (`alab::registry::guard`).
pub fn owner_slug(realm: &str, label: &str) -> Option<String> {
    handle_slug(&slug_word(realm), &handle_of(label))
}

/// The slug account `pk` of `realm` keeps for itself whatever the rows say, `<word>-<16 hex of
/// its key>`, which no handle of another account takes ([`account_slugs`]) and no owner's label
/// makes (`alab::registry::guard`). It never changes, so it is also the label a roster row a
/// sync adds for the account takes: a local key no other account can come to hold.
pub fn reserved_slug(realm: &str, pk: &str) -> String {
    format!(
        "{}-{}",
        slug_word(realm),
        pk.strip_prefix("ac-").unwrap_or(pk)
    )
}

/// The label builds before SF-07 gave account `pk` of `realm`, `<cli>-<6 hex of its key>`, the
/// CLI word being the CLI that signs in to the realm (`claude-1a2b3c`, `codex-…`, `grok-…`,
/// `kimi-…` for both Kimi realms, `agy-…`, `muse-…`). Nothing writes it now; the roster rows
/// Macs renamed to it (from 2026-11-02) keep it until SF-10 re-keys every row by its account
/// key, so the fleet fold resolves a session's word by it (`fleet::fold`) and an owner's label
/// never makes it another account's slug (`alab::registry::guard`). `None` for a realm no CLI
/// signs in to or a key that is no account's.
pub fn earlier_row_label(realm: &str, pk: &str) -> Option<String> {
    let cli = match canonical_realm(realm)? {
        "claude" => "claude",
        "codex" => "codex",
        "grok" => "grok",
        "kimi:global" | "kimi:mainland-cn" => "kimi",
        "google" => "agy",
        "meta" => "muse",
        _ => return None,
    };
    let hex = pk.strip_prefix("ac-").filter(|_| is_digest_pk(pk, "ac"))?;
    Some(format!("{cli}-{}", &hex[..6]))
}

/// The word a slug of `realm` starts with: its provider word, else the realm's own letters and
/// digits cut to [`MAX_WORD`], else `acct`.
fn slug_word(realm: &str) -> String {
    provider_word(realm).map_or_else(
        || {
            let mut word = handle_of(realm).replace('-', "");
            word.truncate(MAX_WORD);
            if word.is_empty() {
                "acct".to_owned()
            } else {
                word
            }
        },
        str::to_owned,
    )
}

/// Every account's slug, by key (design §10): `<provider word>-<handle>`, one short word that
/// names the account in every command, file name and wire, unique across the lab. Provider
/// words map one to one onto realms and none followed by `-` begins another (`kimi-`,
/// `kimicn-`), so slugs unique within each realm are unique across the lab, and two Macs that
/// hold the same history compute the same slugs, before a table roll and after it. Live accounts
/// are named first, so a retired row never takes a live one's slug, then retired ones, each
/// group in three steps:
///
/// 1. **The owner's handles** ([`owner_slug`]). An account the owner named takes its handle.
///    Where two owners' handles make one slug, the write with the lowest operation digest takes
///    it: an order every Mac reads alike, which a roll keeps, since a carried candidate keeps the
///    operation that wrote it. The others are named as if unnamed. This build's owner writes
///    never make such a pair (the registry refuses a label whose slug another account holds,
///    `alab::registry::guard`); concurrent writes and older builds can.
/// 2. **The derived handles**, from the claims address ([`address_handles`]). Two live accounts
///    of one realm that derive one short handle both take the long one, or the key where the
///    long one does not fit, and the short slug names no account, live or retired, while they
///    share it: the slug the first of them held names nothing once a second arrives. A handle
///    whose slug is taken, or that two accounts of this step both want, goes long; a long one
///    taken or wanted twice goes to the key.
/// 3. **The key**: `<word>-<first 6 hex>`, or all 16 where those are taken.
///
/// A handle that is empty or longer than [`MAX_HANDLE`] bytes, an account with no readable
/// address, and a slug the value guards would refuse to show (`guard::withheld`) go to the next
/// step. No handle takes another account's 16-hex slug ([`reserved_slug`]), so every account has
/// one.
///
/// The key never changes; a slug changes when the owner names the account, when the address
/// changes, or when a second account arrives with the same handle.
pub fn account_slugs(accounts: &[Slugged<'_>]) -> BTreeMap<String, String> {
    let words: Vec<String> = accounts
        .iter()
        .map(|account| slug_word(account.realm))
        .collect();
    let digits_of = |at: usize| -> &str {
        let pk = accounts[at].pk;
        pk.strip_prefix("ac-").unwrap_or(pk)
    };
    let fits = |handle: &str| !handle.is_empty() && handle.len() <= MAX_HANDLE;
    // Each account's last resort, which no other account may take.
    let whole: BTreeMap<String, usize> = (0..accounts.len())
        .map(|at| (format!("{}-{}", words[at], digits_of(at)), at))
        .collect();
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    // The slugs no account may take now: each one taken, and each short slug two live accounts
    // share.
    let mut taken: BTreeSet<String> = BTreeSet::new();
    let free = |taken: &BTreeSet<String>, at: usize, slug: &str| {
        !taken.contains(slug)
            && whole.get(slug).is_none_or(|owner| *owner == at)
            && !super::guard::withheld(slug)
    };
    for live in [true, false] {
        let group: Vec<usize> = (0..accounts.len())
            .filter(|at| accounts[*at].live == live)
            .collect();
        // 1. The owner's handles, by slug.
        let mut owned: BTreeMap<String, Vec<(usize, Digest)>> = BTreeMap::new();
        for &at in &group {
            if let Some((label, operation)) = accounts[at].handle
                && let Some(slug) = handle_slug(&words[at], &handle_of(label))
            {
                owned.entry(slug).or_default().push((at, operation));
            }
        }
        let mut named: BTreeSet<usize> = BTreeSet::new();
        for (slug, wanting) in owned {
            // The lowest operation digest, then the lowest key, for one record that named two
            // accounts alike.
            let first = wanting
                .iter()
                .min_by_key(|(at, operation)| (*operation, accounts[*at].pk))
                .map(|(at, _)| *at);
            if let Some(at) = first
                && free(&taken, at, &slug)
            {
                taken.insert(slug.clone());
                out.insert(accounts[at].pk.to_owned(), slug);
                named.insert(at);
            }
        }
        // 2. The derived handles: (the short one, the long one), each when it fits.
        let unnamed: Vec<usize> = group
            .iter()
            .copied()
            .filter(|at| !named.contains(at))
            .collect();
        let derived: BTreeMap<usize, (Option<String>, Option<String>)> = unnamed
            .iter()
            .map(|&at| {
                let pair = accounts[at].claims_email.and_then(address_handles).map_or(
                    (None, None),
                    |(short, long)| {
                        (
                            Some(short).filter(|h| fits(h)),
                            Some(long).filter(|h| fits(h)),
                        )
                    },
                );
                (at, pair)
            })
            .collect();
        // Every live account of a realm that derives a short handle counts toward it, whether or
        // not its long handle fits.
        let mut sharing: BTreeMap<(&str, &str), usize> = BTreeMap::new();
        if live {
            for (at, (short, _)) in &derived {
                if let Some(short) = short {
                    *sharing
                        .entry((words[*at].as_str(), short.as_str()))
                        .or_default() += 1;
                }
            }
        }
        for ((word, short), count) in &sharing {
            if *count > 1 {
                taken.insert(format!("{word}-{short}"));
            }
        }
        let mut choices: BTreeMap<usize, Vec<String>> = BTreeMap::new();
        for (at, (short, long)) in &derived {
            let shared = short.as_ref().is_some_and(|short| {
                sharing
                    .get(&(words[*at].as_str(), short.as_str()))
                    .is_some_and(|count| *count > 1)
            });
            let handles: Vec<&String> = if shared {
                long.iter().collect()
            } else {
                short.iter().chain(long).collect()
            };
            choices.insert(
                *at,
                handles
                    .into_iter()
                    .map(|handle| format!("{}-{handle}", words[*at]))
                    .collect(),
            );
        }
        // Each round offers every account still unnamed its next choice; a slug taken, or wanted
        // by two of them in one round, is no account's in that round.
        let depth = choices.values().map(Vec::len).max().unwrap_or(0);
        for round in 0..depth {
            let mut wanted: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
            for (at, slugs) in &choices {
                if !out.contains_key(accounts[*at].pk)
                    && let Some(slug) = slugs.get(round)
                {
                    wanted.entry(slug).or_default().push(*at);
                }
            }
            for (slug, wanting) in wanted {
                if let [at] = wanting.as_slice()
                    && free(&taken, *at, slug)
                {
                    taken.insert(slug.to_owned());
                    out.insert(accounts[*at].pk.to_owned(), slug.to_owned());
                }
            }
        }
        // 3. The key.
        for at in unnamed {
            if out.contains_key(accounts[at].pk) {
                continue;
            }
            let digits = digits_of(at);
            let short = format!("{}-{}", words[at], digits.get(..6).unwrap_or(digits));
            let slug = if free(&taken, at, &short) {
                short
            } else {
                format!("{}-{digits}", words[at])
            };
            taken.insert(slug.clone());
            out.insert(accounts[at].pk.to_owned(), slug);
        }
    }
    out
}

/// What [`account_slugs`] settled that `aterm db alab check` reports (`alab::checks`), each by
/// key and never by value, over live accounts alone: a retired row takes what the live ones
/// leave by design.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SlugFindings {
    /// Each set of live accounts of one provider, none named by its owner, that derive one short
    /// handle: they took the long one, or the key, and the short slug names none of them.
    pub shared: Vec<Vec<String>>,
    /// Each live account whose owner's label names it nothing, and the account that holds the
    /// slug the label makes, where one does: an earlier owner's label, or a key's own slug.
    pub lost: Vec<(String, Option<String>)>,
    /// Each live account whose owner's label starts with its own provider word, so its slug
    /// says the word twice (`claude-claude-work`). It names the account; it is reported, never
    /// refused.
    pub repeats: Vec<String>,
}

/// The findings of `slugs`, the slugs [`account_slugs`] gave `accounts`.
#[must_use]
pub fn slug_findings(accounts: &[Slugged<'_>], slugs: &BTreeMap<String, String>) -> SlugFindings {
    let mut found = SlugFindings::default();
    let holder: BTreeMap<&str, &str> = slugs
        .iter()
        .map(|(pk, slug)| (slug.as_str(), pk.as_str()))
        .collect();
    let reserved: BTreeMap<String, &str> = accounts
        .iter()
        .map(|account| (reserved_slug(account.realm, account.pk), account.pk))
        .collect();
    let mut sharing: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for account in accounts.iter().filter(|account| account.live) {
        let word = slug_word(account.realm);
        let slug = slugs.get(account.pk).map(String::as_str);
        let mut named = false;
        if let Some((label, _)) = account.handle {
            let handle = handle_of(label);
            if handle == word || handle.starts_with(&format!("{word}-")) {
                found.repeats.push(account.pk.to_owned());
            }
            let wanted = handle_slug(&word, &handle);
            named = wanted.is_some() && wanted.as_deref() == slug;
            if !named {
                let held = wanted.as_deref().and_then(|wanted| {
                    holder
                        .get(wanted)
                        .copied()
                        .or_else(|| reserved.get(wanted).copied())
                        .filter(|other| *other != account.pk)
                });
                found
                    .lost
                    .push((account.pk.to_owned(), held.map(str::to_owned)));
            }
        }
        // An account its owner's label does not name derives its slug, as `account_slugs`'s
        // second step does.
        if !named
            && let Some((short, _)) = account.claims_email.and_then(address_handles)
            && !short.is_empty()
            && short.len() <= MAX_HANDLE
        {
            sharing
                .entry((word, short))
                .or_default()
                .push(account.pk.to_owned());
        }
    }
    found.shared = sharing.into_values().filter(|pks| pks.len() > 1).collect();
    found
}

/// An import source's key: a digest of its bytes, so a re-run of the same file collapses.
pub fn source_pk(bytes: &[u8]) -> String {
    prefix_hex("sr", &Sha256::digest(bytes)[..8])
}

/// Lower-case Crockford base32: no `i`, `l`, `o` or `u`.
const CROCKFORD: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// The characters of a text set's element key.
pub const TEXT_ELEM: usize = 16;

/// A text set element's normal form: its surrounding whitespace trimmed and each inner run of
/// whitespace one space. Case is kept: an element shows as it was written.
pub fn normal_text(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The element key of a text set's member (`Ty::Set(&Ty::Text(_))`): 16 lower-case Crockford
/// base32 characters of a SHA-256 of its normal form ([`normal_text`]), so a host name, a domain
/// or an alias, whose dots and length break the key grammar, is one register however many Macs
/// add it.
pub fn text_elem(text: &str) -> String {
    let mut sha = Sha256::new();
    sha.update(b"aterm/att/text-elem/1\0");
    sha.update(normal_text(text).as_bytes());
    let digest = sha.finalize();
    let mut bits = 0u128;
    for &byte in &digest[..TEXT_ELEM * 5 / 8] {
        bits = (bits << 8) | u128::from(byte);
    }
    (0..TEXT_ELEM)
        .rev()
        .map(|n| char::from(CROCKFORD[((bits >> (n * 5)) & 31) as usize]))
        .collect()
}

/// Whether `elem` is a text set's element key in form ([`text_elem`]).
pub fn is_text_elem(elem: &str) -> bool {
    elem.len() == TEXT_ELEM && elem.bytes().all(|b| CROCKFORD.contains(&b))
}

/// An archived owned record's key in a later table generation (design §3.8): a digest of the
/// operation it carries, so every Mac that carries one record names one row.
pub fn archived_pk(operation: &[u8; 32]) -> String {
    h8("ar", b"aterm/alab/archived/1\0", &[operation])
}

/// An account event's row (SF-47, `alab::registry::ACCOUNT_EVENT`): a digest of its subject
/// (an `ac-` account, or an `ad-` login where the event names no account), the source word, the
/// cause word, the status it moved to (empty for a note) and `discriminator`, so a re-import of
/// one sheet cell names the same row whatever else the sheet's tab says. An event that cannot
/// collapse (a person's or an agent's note) passes what makes it one write, its operation or
/// time; an import passes nothing.
///
/// # Errors
/// `subject` is neither an account key nor an address key.
pub fn account_event_pk(
    subject: &str,
    source: &str,
    cause: &str,
    to: &str,
    discriminator: &[u8],
) -> Result<String> {
    if !(is_digest_pk(subject, "ac") || is_digest_pk(subject, "ad")) {
        return Err(Error::new(
            "an account event names an account or a login address",
        ));
    }
    Ok(h8(
        "ae",
        b"aterm/alab/account-event/1\0",
        &[
            subject.as_bytes(),
            source.as_bytes(),
            cause.as_bytes(),
            to.as_bytes(),
            discriminator,
        ],
    ))
}

/// A carried history entry's row (`alab::history`), keyed by the Mac that carried it and the
/// operation it restates: two Macs carrying one operation write two rows, so no row is a
/// register of two candidates that a later restatement would have to carry together.
pub fn history_pk(uuid32: &str, operation: &[u8; 32]) -> String {
    h8(
        "hy",
        b"aterm/alab/history/2\0",
        &[uuid32.as_bytes(), operation.as_slice()],
    )
}

/// One roller's history horizon for generation `from` (`alab::history`): what its bound left out.
/// The row names both, and a horizon is read only under the key they make.
pub fn history_horizon_pk(from: u32, uuid32: &str) -> String {
    h8(
        "hy",
        b"aterm/alab/history-horizon/1\0",
        &[&from.to_le_bytes(), uuid32.as_bytes()],
    )
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

#[cfg(test)]
#[path = "keys_slug_tests.rs"]
mod slug_tests;
