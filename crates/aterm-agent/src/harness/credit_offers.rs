// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The free credit offers Claude Code has cached for the signed-in account, read by an
//! allowlist from its profile (`.claude.json`, through [`super::profile`]'s bounded reader).
//!
//! The owner (2026-10-26): *"if there is a link like 'claim credits' then go have an agent go
//! to that link and go claim them or whatever. the harness should do this."* The window's host
//! starts one worker that claims a live offer in the person's own Chrome, where claude.ai is
//! signed in ([`claim_task`]). This module only reads: it calls no vendor endpoint, and keeps
//! nothing of the file but the few keys below.
//!
//! # What is claimed
//!
//! Two offers, as Claude Code 2.1.311 reads them:
//!
//! - [`OfferId::CloudCredit`]: the flag `cachedGrowthBookFeatures.tengu_swift_lynx`
//!   (`endpoint`, `webPath`, `claimLinkIncludesOrganization`, `startup.claimPageLine` /
//!   `startup.text`) and its status `promoStartupStatusCache` (`accountUuid`, `endpoint`,
//!   `eligible`, `claimed`). Live when the status names the signed-in account
//!   (`oauthAccount.accountUuid`) and the flag's endpoint, `eligible` is `true` and `claimed` is
//!   `false`. The link is the one Claude Code's `/claim-credit` opens:
//!   `https://claude.ai` + `webPath` + `/` + [`CLAIM_LINK_VARIANT`], then `/` + the lowercased
//!   `oauthAccount.organizationUuid` where `claimLinkIncludesOrganization` is `true`. The grant
//!   happens on that page; the CLI makes no claim call, so a signed-in browser is needed.
//! - [`OfferId::ApiCredits`]: the flag `cachedGrowthBookFeatures.tengu_snug_storm.text`
//!   (1 to 240 characters, naming a link) and its status
//!   `apiCreditLinkStatusCache[<organizationUuid>].eligibleSince`. Live when that is a number.
//!   The link is the `https://` link in the text, on a host of [`VENDOR_HOSTS`].
//!
//! # What is never claimed, and why
//!
//! - Feature of the week (`tengu_lilac_loom`, `fotwEligibilityCache`): its slash command posts
//!   `overage_credit_grant` with `enable_overages: true`, which turns extra-usage billing on.
//! - Guest passes: they need other people to subscribe.
//! - "Fable is on us": an admin's opt-in, with nothing to claim.
//!
//! None of their keys is read here.
//!
//! # The worker's fence
//!
//! The worker starts in [`CLAIM_FOLDER`] under the harness state directory, with every box
//! raised (`--permission-mode default`, whatever the person's settings say), and the supervisor
//! knows it by that trusted folder ([`is_claim_folder`]). Its approval rule (`RULE_CLAIM_WORKER`
//! in `supervise::policy::approval`) passes only a Claude in Chrome action pressed once (never
//! the site-wide grant) on a vendor page [`claim_page`] admits and that tool's own reads
//! ([`CHROME_TOOL`]); it hands every question to a person and declines every other tool's box.
//! Its task ends in the one reply Autopilot Land lands on (`Nothing left:`), and the window's
//! host turns its Autopilot Off once it has run 30 minutes.
//!
//! # What is never read
//!
//! The file's other keys are skipped unmaterialized, as [`super::profile`]'s allowlist skips
//! them: `emailAddress`, `displayName`, `fullName` and `organizationName` are never read or
//! kept. An offer keeps its vendor line, its link and a digest of the account, the
//! organization, the offer and the link ([`OfferKey`]).

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Deserialize;
use serde::de::{self, IgnoredAny, MapAccess, SeqAccess, Visitor};

use super::profile::{Address, Leaf, NoProfile, Obj, drain_seq};

/// The path segment Claude Code 2.1.311's `/claim-credit` puts after the flag's `webPath`, a
/// constant there (measured in its bundle).
pub const CLAIM_LINK_VARIANT: &str = "6";

/// The hosts an offer's link may name: the vendor's own.
pub const VENDOR_HOSTS: &[&str] = &[
    "clau.de",
    "claude.ai",
    "claude.com",
    "console.anthropic.com",
    "platform.claude.com",
    "anthropic.com",
];

/// The folder a claim worker starts in, under the harness state directory
/// (`<aterm state>/harness/`[`super::cli::HARNESS`]`/credit-claim`), which its task record names
/// as its trusted folder ([`is_claim_folder`]).
pub const CLAIM_FOLDER: &str = "credit-claim";

/// How Claude Code's Tool box names a call of a Claude in Chrome tool: Claude Code 2.1.311 gives
/// the built-in `claude-in-chrome` server's tools the name `Claude in Chrome[<tool>]` (its
/// `getClaudeInChromeMCPToolOverrides`, `_mcp` dropped), and the box draws an MCP call as that
/// name, ` Tool:` and ` (MCP)`, as it draws `github - create_issue Tool: (MCP)` for any other
/// server's (measured in its bundle). Another server's tool, one named `claude-in-chrome`
/// included, reads `<server> - <tool>`. Its site actions raise Claude in Chrome's own box
/// instead ([`claim_page`]).
pub const CHROME_TOOL: &str = "Claude in Chrome[";

/// The words a page a claim worker acts on may not hold after its host, lowercased, anywhere in
/// its path, query or fragment: the pages that bill, sell, change a plan or a spend limit, or
/// show usage. A claim link holds none of them (its path is a `webPath` and a uuid).
pub const MONEY_WORDS: &[&str] = &[
    "billing", "payment", "checkout", "upgrade", "subscri", "plan", "pricing", "purchase", "buy",
    "usage", "overage", "spend", "limit", "invoice",
];

/// The longest vendor line an offer keeps ([`Offer::words`]), in characters.
pub const WORDS_MAX: usize = 120;

/// The longest `tengu_snug_storm.text` read as an offer, in characters (Claude Code's bound).
const API_TEXT_MAX: usize = 240;

/// The longest link an offer keeps, in bytes.
const URL_MAX: usize = 256;

/// The longest `webPath` read, in bytes.
const WEB_PATH_MAX: usize = 128;

/// The longest id read (an account or organization uuid).
const ID_MAX: usize = 128;

/// The most `apiCreditLinkStatusCache` entries kept for the organization lookup.
const STATUS_ENTRIES_MAX: usize = 32;

const KEY_DOMAIN: &[u8] = b"aterm-credit-offer-v1";

/// Which offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OfferId {
    /// The cloud session credit (`tengu_swift_lynx`, `/claim-credit`).
    CloudCredit,
    /// The plan's monthly API credits (`tengu_snug_storm`).
    ApiCredits,
}

impl OfferId {
    /// `cloud-credit` or `api-credits`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            OfferId::CloudCredit => "cloud-credit",
            OfferId::ApiCredits => "api-credits",
        }
    }

    /// The offer's name where its vendor line does not read as one ([`Offer::words`]).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            OfferId::CloudCredit => "a cloud session credit",
            OfferId::ApiCredits => "the plan's monthly API credits",
        }
    }
}

/// One offer's identity for dedupe: a digest of the account uuid, the organization uuid, the
/// offer and its link, holding none of them.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OfferKey(String);

impl OfferKey {
    fn of(account: &str, org: &str, id: OfferId, url: &str) -> OfferKey {
        let mut hash = aterm_digest::Sha256::new();
        hash.update(KEY_DOMAIN);
        for part in [account, org, id.word(), url] {
            hash.update([0]);
            hash.update((part.len() as u64).to_be_bytes());
            hash.update(part);
        }
        let full = hash.finalize();
        let mut out = String::with_capacity(32);
        aterm_codec::hex::push_lower(&mut out, full.get(..16).unwrap_or(&[]));
        OfferKey(out)
    }

    /// A key back from its text ([`Self::as_str`]): 32 lowercase hex digits.
    #[must_use]
    pub fn parse(text: &str) -> Option<OfferKey> {
        aterm_codec::hex::is_lower(text, 32).then(|| OfferKey(text.to_owned()))
    }

    /// The key's text, 32 lowercase hex digits.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for OfferKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for OfferKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// One live offer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    /// Which offer.
    pub id: OfferId,
    /// The vendor's line for it, printable and at most [`WORDS_MAX`] characters, else the
    /// offer's [`OfferId::name`].
    pub words: String,
    /// The link that claims it: `https://`, on a host of [`VENDOR_HOSTS`].
    pub url: String,
    /// [`OfferKey`].
    pub key: OfferKey,
}

/// What a file's size and modification time read, for a look that parses nothing when they
/// stand ([`stamp`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    /// The file's length.
    pub len: u64,
    /// Its modification time.
    pub modified: Option<SystemTime>,
}

/// The [`Stamp`] of the file at `path`, `None` where none can be read.
#[must_use]
pub fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some(Stamp {
        len: meta.len(),
        modified: meta.modified().ok(),
    })
}

/// The profile Claude Code reads for the home a process's exec environment `env` names
/// (`CLAUDE_CONFIG_DIR`, the home override and the custom-OAuth selector, as
/// [`super::profile::account_of`] resolves it), under the user's home `user_home`.
#[must_use]
pub fn path_for(env: &[String], user_home: &Path) -> Option<PathBuf> {
    super::profile::effective_of(env, user_home)
}

/// The live offers in the profile at `path` ([`parse`]), read as [`super::profile::read`] reads.
/// # Errors
/// [`NoProfile`]: why nothing was read.
pub fn read(path: &Path) -> Result<Vec<Offer>, NoProfile> {
    super::profile::read_with(path, parse)
}

/// The live offers in a profile's bytes, [`OfferId::CloudCredit`] first.
/// # Errors
/// [`NoProfile::Unreadable`] for bytes that are not one JSON object.
pub fn parse(bytes: &[u8]) -> Result<Vec<Offer>, NoProfile> {
    let Obj(raw) =
        aterm_json::from_slice::<Obj<RawFile>>(bytes).map_err(|_| NoProfile::Unreadable)?;
    let raw = raw.ok_or(NoProfile::Unreadable)?;
    Ok(offers_of(raw))
}

fn offers_of(raw: RawFile) -> Vec<Offer> {
    let oauth = raw.oauth.0.unwrap_or_default();
    let Some(account) = id_of(oauth.account.0) else {
        return Vec::new();
    };
    let org = id_of(oauth.organization.0);
    let features = raw.features.0.unwrap_or_default();
    let mut out = Vec::new();
    if let Some(offer) = cloud_credit(
        &account,
        org.as_deref(),
        features.cloud.0,
        raw.promo.0.unwrap_or_default(),
    ) {
        out.push(offer);
    }
    if let Some(org) = org.as_deref()
        && let Some(offer) = api_credits(&account, org, features.api.0, &raw.api_status)
    {
        out.push(offer);
    }
    out
}

/// An id as read: non-empty, at most [`ID_MAX`] bytes, printable ASCII.
fn id_of(text: Option<String>) -> Option<String> {
    text.filter(|t| !t.is_empty() && t.len() <= ID_MAX && t.bytes().all(|b| b.is_ascii_graphic()))
}

fn cloud_credit(
    account: &str,
    org: Option<&str>,
    flag: Option<RawCloudFlag>,
    status: RawPromoStatus,
) -> Option<Offer> {
    let flag = flag?;
    let endpoint = flag.endpoint.0.filter(|e| !e.is_empty())?;
    let live = status.account.0.as_deref() == Some(account)
        && status.endpoint.0.as_deref() == Some(endpoint.as_str())
        && status.eligible.flag() == Some(true)
        && status.claimed.flag() == Some(false);
    if !live {
        return None;
    }
    let web_path = flag.web_path.0.filter(|p| web_path_ok(p))?;
    let mut url = format!("https://claude.ai{web_path}/{CLAIM_LINK_VARIANT}");
    if flag.with_org.flag() == Some(true) {
        // Claude Code drops the segment for an id that is no uuid; the harness claims for no
        // organization it cannot name, so such an offer is not taken.
        let org = org.map(str::to_ascii_lowercase).filter(|o| uuid_ok(o))?;
        url.push('/');
        url.push_str(&org);
    }
    let url = vendor_url(&url)?;
    let startup = flag.startup.0.unwrap_or_default();
    let words = [startup.claim_page_line.0, startup.text.0]
        .into_iter()
        .flatten()
        .find_map(|line| words_ok(&line))
        .unwrap_or_else(|| OfferId::CloudCredit.name().to_owned());
    Some(Offer {
        id: OfferId::CloudCredit,
        key: OfferKey::of(account, org.unwrap_or(""), OfferId::CloudCredit, &url),
        words,
        url,
    })
}

fn api_credits(
    account: &str,
    org: &str,
    flag: Option<RawApiFlag>,
    statuses: &OrgStatuses,
) -> Option<Offer> {
    let eligible = statuses.0.iter().any(|(key, number)| key == org && *number);
    if !eligible {
        return None;
    }
    let text = flag?.text.0?;
    let chars = text.chars().count();
    if chars == 0 || chars > API_TEXT_MAX {
        return None;
    }
    let (at, url) = link_in(&text)?;
    let words = api_words(text.get(..at).unwrap_or(""))
        .unwrap_or_else(|| OfferId::ApiCredits.name().to_owned());
    Some(Offer {
        id: OfferId::ApiCredits,
        key: OfferKey::of(account, org, OfferId::ApiCredits, &url),
        words,
        url,
    })
}

/// The first `https://` link in `text` and where it starts, cut at the first space and its
/// closing punctuation dropped, where it passes [`vendor_url`].
fn link_in(text: &str) -> Option<(usize, String)> {
    let at = text.find("https://")?;
    let rest = text.get(at..)?;
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let link = rest
        .get(..end)?
        .trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '\'', '"']);
    Some((at, vendor_url(link)?))
}

/// The sentences of an API-credits line before its link: up to its last full stop, else all of
/// it, where [`words_ok`] takes it.
fn api_words(before: &str) -> Option<String> {
    let before = before.trim_end();
    let sentences = before
        .rfind('.')
        .map_or(before, |at| before.get(..=at).unwrap_or(before));
    words_ok(sentences)
}

/// `^(/[a-z0-9_-]+)+$`, as Claude Code requires of a `webPath`, at most [`WEB_PATH_MAX`] bytes.
fn web_path_ok(path: &str) -> bool {
    path.len() <= WEB_PATH_MAX
        && path.starts_with('/')
        && path.split('/').skip(1).all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        })
}

/// `^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`.
fn uuid_ok(id: &str) -> bool {
    let groups: Vec<&str> = id.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(group, len)| aterm_codec::hex::is_lower(group, len))
}

/// `link` where it is one an offer may name: `https://`, at most [`URL_MAX`] bytes of
/// printable ASCII with no space, quote, angle bracket, backslash, backtick, brace, pipe or
/// caret, its host one of [`VENDOR_HOSTS`] exactly, with no user or port.
#[must_use]
pub fn vendor_url(link: &str) -> Option<String> {
    if link.len() > URL_MAX
        || !link
            .bytes()
            .all(|b| b.is_ascii_graphic() && !b"\"'<>\\`{}|^".contains(&b))
    {
        return None;
    }
    let rest = link.strip_prefix("https://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = rest.get(..end)?;
    VENDOR_HOSTS.contains(&host).then(|| link.to_owned())
}

/// Whether `p`, a session's trusted folder (`meta trusted_cwd=`), is a claim worker's: its last
/// component is [`CLAIM_FOLDER`] and its parent's is the harness state directory's, read by
/// the two names as the disk-free worker's folder is ([`super::disk::fence::is_harness_folder`]).
#[must_use]
pub fn is_claim_folder(p: &Path) -> bool {
    super::disk::fence::is_harness_folder(p, CLAIM_FOLDER)
}

/// Whether `host`, as a page's link spells it, is one of [`VENDOR_HOSTS`] or a name under one: the
/// vendor's host exactly, or that host after a `.` (`www.claude.ai`), in lowercase letters,
/// digits, `-` and `.` only, with no empty label.
#[must_use]
pub fn vendor_host(host: &str) -> bool {
    let plain = !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
        && host.split('.').all(|label| !label.is_empty());
    plain
        && VENDOR_HOSTS.iter().any(|vendor| {
            host == *vendor
                || host
                    .strip_suffix(vendor)
                    .is_some_and(|sub| sub.len() > 1 && sub.ends_with('.'))
        })
}

/// Why the page at `url` is none a claim worker may act on, `Ok` where it is one: an `https://`
/// link of printable ASCII, its host a [`vendor_host`] with no user (`@`) and no port (`:`), and
/// nothing after the host (path, query and fragment, lowercased) holding a word of
/// [`MONEY_WORDS`].
///
/// # Errors
///
/// Why not, in a few words for the worker and the person.
pub fn claim_page(url: &str) -> Result<(), String> {
    if url.is_empty() {
        return Err("Claude in Chrome's box names no page".to_owned());
    }
    if !url.bytes().all(|b| b.is_ascii_graphic()) {
        return Err("its page's link is not one plain line".to_owned());
    }
    let Some(rest) = url.strip_prefix("https://") else {
        return Err("its page is not an https:// page".to_owned());
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, after) = rest.split_at(end);
    if authority.contains('@') {
        return Err("its page's link names a user before the host".to_owned());
    }
    if authority.contains(':') {
        return Err("its page's link names a port".to_owned());
    }
    if !vendor_host(authority) {
        return Err(format!("{authority} is not the vendor's host"));
    }
    let after = after.to_ascii_lowercase();
    if let Some(word) = MONEY_WORDS.iter().find(|w| after.contains(*w)) {
        return Err(format!(
            "its page's link names `{word}`: a billing, plan, usage or payment page"
        ));
    }
    Ok(())
}

/// `line` as an offer's words: trimmed, 1 to [`WORDS_MAX`] characters, with no control
/// character, line or paragraph separator, or bidirectional control.
fn words_ok(line: &str) -> Option<String> {
    let line = line.trim();
    let chars = line.chars().count();
    let printable = line.chars().all(|c| {
        !c.is_control()
            && !matches!(
                c,
                '\u{2028}' | '\u{2029}' | '\u{200e}' | '\u{200f}' | '\u{061c}'
            )
            && !('\u{202a}'..='\u{202e}').contains(&c)
            && !('\u{2066}'..='\u{2069}').contains(&c)
    });
    (printable && chars > 0 && chars <= WORDS_MAX).then(|| line.to_owned())
}

/// The task a claim worker is given: one string, opening with the harness's mark, naming the
/// offer's link and words, what the worker must never do, and the one reply it ends with, which
/// Autopilot Land lands on (`Nothing left:` with no NEEDS YOU line).
#[must_use]
pub fn claim_task(offer: &Offer) -> String {
    format!(
        concat!(
            harness_mark!(),
            " Claim a free Claude credit for this account. Use your Claude in Chrome tools to \
             open {url} in the person's own Chrome, where claude.ai is signed in, and claim the \
             free credit the page offers ({words}). Act only on the claim page and the pages \
             its claim leads to; never open billing, plan, usage or payment settings. Never \
             enter, add or change payment details; never buy anything; never turn on extra \
             usage or change the plan, billing or spend limits; never start a paid trial; never \
             sign in with or create new credentials. Never ask a question. If the page asks for \
             any of that, or the Chrome tools are not available, stop. There is nothing to \
             commit, push or land. End with one reply starting `Nothing left:` that says \
             CLAIMED: <what was claimed> or NOT CLAIMED: <why>, and no NEEDS YOU line: the \
             person reads your reply in this tab."
        ),
        url = offer.url,
        words = offer.words,
    )
}

/// The file: the four keys read; every other key skipped.
#[derive(Default, Deserialize)]
#[serde(default)]
struct RawFile {
    #[serde(rename = "oauthAccount")]
    oauth: Obj<RawOauth>,
    #[serde(rename = "cachedGrowthBookFeatures")]
    features: Obj<RawFeatures>,
    #[serde(rename = "promoStartupStatusCache")]
    promo: Obj<RawPromoStatus>,
    #[serde(rename = "apiCreditLinkStatusCache")]
    api_status: OrgStatuses,
}

/// `oauthAccount`: the two ids alone (never its address or names).
#[derive(Default, Deserialize)]
#[serde(default)]
struct RawOauth {
    #[serde(rename = "accountUuid")]
    account: Address,
    #[serde(rename = "organizationUuid")]
    organization: Address,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawFeatures {
    #[serde(rename = "tengu_swift_lynx")]
    cloud: Obj<RawCloudFlag>,
    #[serde(rename = "tengu_snug_storm")]
    api: Obj<RawApiFlag>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawCloudFlag {
    endpoint: Address,
    #[serde(rename = "webPath")]
    web_path: Address,
    #[serde(rename = "claimLinkIncludesOrganization")]
    with_org: Leaf,
    startup: Obj<RawStartup>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawStartup {
    text: Address,
    #[serde(rename = "claimPageLine")]
    claim_page_line: Address,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawApiFlag {
    text: Address,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawPromoStatus {
    #[serde(rename = "accountUuid")]
    account: Address,
    endpoint: Address,
    eligible: Leaf,
    claimed: Leaf,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawApiStatus {
    #[serde(rename = "eligibleSince")]
    since: Leaf,
}

/// `apiCreditLinkStatusCache`: each organization's key and whether its `eligibleSince` is a
/// number, at most [`STATUS_ENTRIES_MAX`] entries, each key at most [`ID_MAX`] bytes; anything
/// else read past, unkept.
#[derive(Default)]
struct OrgStatuses(Vec<(String, bool)>);

impl<'de> Deserialize<'de> for OrgStatuses {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = OrgStatuses;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any value")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<OrgStatuses, A::Error> {
                let mut out = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    if out.len() >= STATUS_ENTRIES_MAX || key.is_empty() || key.len() > ID_MAX {
                        map.next_value::<IgnoredAny>()?;
                        continue;
                    }
                    let Obj(status) = map.next_value::<Obj<RawApiStatus>>()?;
                    let number = status.is_some_and(|s| s.since.num().is_some_and(f64::is_finite));
                    out.push((key, number));
                }
                Ok(OrgStatuses(out))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<OrgStatuses, A::Error> {
                drain_seq(seq).map(|()| OrgStatuses::default())
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<OrgStatuses, E> {
                Ok(OrgStatuses::default())
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<OrgStatuses, E> {
                Ok(OrgStatuses::default())
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<OrgStatuses, E> {
                Ok(OrgStatuses::default())
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<OrgStatuses, E> {
                Ok(OrgStatuses::default())
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<OrgStatuses, E> {
                Ok(OrgStatuses::default())
            }
            fn visit_unit<E: de::Error>(self) -> Result<OrgStatuses, E> {
                Ok(OrgStatuses::default())
            }
            fn visit_none<E: de::Error>(self) -> Result<OrgStatuses, E> {
                Ok(OrgStatuses::default())
            }
        }
        d.deserialize_any(V)
    }
}

#[cfg(test)]
#[path = "credit_offers_tests.rs"]
mod tests;
