// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CLAUDE CODE'S PROFILE, READ BY AN ALLOWLIST
//! (`docs/DESIGN-account-stats-2026-10-15.md` §3.1, §3.3, §4.2): what the
//! `.claude.json` Claude Code keeps beside or inside a home says about the
//! account it is signed in to — the plan and its tiers, whether usage
//! credits are on, the `/usage` body it caches, the two flags that say how
//! Fable is billed — and the account's [`AccountKey`].
//!
//! THE SAME FILE CAN HOLD CREDENTIALS (design F17): an API key under
//! `primaryApiKey`, `customApiKeyResponses`, and `mcpServers` — at the top
//! level and under every project — whose `env` and `headers` often carry
//! tokens. So it is never parsed whole. [`parse`] is a typed reader of the
//! keys below and no others: the value of every other key is skipped as the
//! parser scans past it (`aterm_json`'s ignored-value path, which checks a
//! string without decoding it), so none is ever built as a value — no
//! `aterm_json::Value` of the file exists. A kept field of the wrong type
//! reads as absent and the rest still reads; a file that is not JSON is
//! [`NoProfile::Unreadable`], one word, never the parser's message. [`read`]
//! zeroes the file's bytes before it frees them, so what was skipped does
//! not stay behind in freed memory either. `tests/profile_sentinels.rs`
//! checks typed-output canaries and bounds allocation requests for large escaped
//! secrets, with a materializing-reader negative control; it never scans freed
//! or potentially uninitialized allocation contents.
//!
//! THE KEYS READ, by their names in Claude Code 2.1.301 (READ in its
//! bundle; names only were surveyed on disk):
//!
//! * `oauthAccount`: `accountUuid` (hashed as it is read: [`AccountKey`]),
//!   `organizationType`, `userRateLimitTier`, `organizationRateLimitTier`,
//!   `seatTier`, `billingType`, `subscriptionCreatedAt`,
//!   `hasExtraUsageEnabled`, `planDisplayName`, `profileFetchedAt`. Never its
//!   `displayName`, `fullName`, `organizationName` or `organizationUuid`,
//!   and its `emailAddress` only in the in-memory login check [`answers_to`]
//!   and the explicitly requested, volatile footer display [`subscription_for`].
//!   The general [`Profile`] and account-stat records retain no email.
//! * `cachedExtraUsageDisabledReason`: [`Credits`].
//! * `cachedUsageUtilization`: `fetchedAtMs`, `accountUuid` (hashed, and the
//!   body is dropped unless it names the profile's own account, as Claude
//!   Code drops it) and the usage body's windows, its per-model weekly rows,
//!   its usage-credits spend and its two one-time credit blocks
//!   ([`UsageCache`]).
//! * `cachedGrowthBookFeatures`: the two Fable billing flags by name,
//!   `tengu_saffron_credits_only_tiers` and `tengu_saffron_lattice`
//!   ([`FablePlan`]), and `tengu_calm_seal`, the model families billed to
//!   usage credits only ([`Profile::credits_only`]). Their names stay in this
//!   file: what leaves it is aterm's own word.
//! * `fableOverageConsentV2` and `usageCreditsOnlyModelConsent`: the
//!   consents Claude Code stores to bill a model to usage credits with no
//!   dialog (READ in the 2.1.304 bundle: `{ "<org or acct:id>": true }`, the
//!   second per family), as the families consented
//!   ([`Profile::consented`]). The ids they are keyed by are read past,
//!   never kept.
//!
//! WHERE THE FILE IS: the home's own profile is [`path_of`], by the one
//! resolver of a Claude home (`upgrade_drive`'s `Records`):
//! `CLAUDE_CONFIG_DIR`'s rule, the process's own `HOME`, and an empty
//! `CLAUDE_CONFIG_DIR`'s profile in `$HOME`. The file Claude Code reads for
//! that home is [`effective`]: a legacy `.config.json` in its records
//! directory can stand in for it, and a custom OAuth URL renames it.
//!
//! What this module answers is a record (design §8.2): it reads and
//! decides nothing, and nothing that decides — no supervisor, no rotation —
//! reads it, but for THREE keys: whether a login signed in again in its tab
//! is its roster row's own account ([`answers_to`], a yes or a no), the
//! method `/login`'s picker is answered with
//! ([`login_method`], a plan word and nothing else), and under `[harness]
//! usage_credits = false` which models it bills to usage credits
//! ([`FablePlan`], [`Profile::consented`], [`Profile::credits_only`]): such a
//! model is moved onto by no upgrade, fallback or return
//! (`upgrade_drive::credits_billed_for`, `upgrade_models::credits_billed`;
//! the owner, 2026-10-19) — and where the profile cannot say (no profile
//! read, a plan not known, two logins listed), Fable is taken for billed:
//! the review of 2026-10-19, P4. Its [`AccountKey`] and [`Word`] types are shared: the Codex
//! rollout reader (`ai::codex_limits`) keys each usage reading by the
//! account its rollout names, and keeps the vendor's words, in them.

use std::fmt;
use std::io::Read as _;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde::de::{self, IgnoredAny, MapAccess, SeqAccess, Visitor};

use super::upgrade_drive::{HomeVars, Records};

/// The most of a `.claude.json` that is read. A larger one is
/// [`NoProfile::TooLarge`], unread. The file holds the profile, caches and
/// one entry per project the person opened; surveyed, a machine's was about
/// 100 KB.
pub const PROFILE_MAX: u64 = 32 * 1024 * 1024;

/// The longest text kept from the file ([`Word`]).
pub const WORD_MAX: usize = 64;

/// The longest id hashed into an [`AccountKey`]: a uuid is 36 bytes.
const ID_MAX: usize = 128;

/// What the account key's hash starts with (design §3.1).
const KEY_DOMAIN: &[u8] = b"aterm-account-v1";

/// The digest bytes a key keeps: twelve hex digits.
const KEY_BYTES: usize = 6;

// ---------------------------------------------------------------------------
// The key
// ---------------------------------------------------------------------------

/// ONE ACCOUNT'S KEY, the same on every machine and holding no secret
/// (design §3.1): `<c|x>-` and the first twelve hex digits of
/// `sha256("aterm-account-v1" 0 agent 0 id)`. The id is the account's own
/// non-secret id — Claude Code's `oauthAccount.accountUuid`, Codex's
/// `creator_account_id` 0 `creator_user_id` — and it goes straight into the
/// hash: no key keeps it, and nothing prints it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AccountKey {
    /// `c` for Claude Code, `x` for Codex.
    agent: u8,
    /// The digest's first [`KEY_BYTES`].
    digest: [u8; KEY_BYTES],
}

impl AccountKey {
    /// The key of the Claude Code account whose `accountUuid` is `id`;
    /// `None` for an empty or overlong id.
    #[must_use]
    pub fn claude(id: &str) -> Option<AccountKey> {
        Self::hashed(b'c', "claude", &[id])
    }

    /// The key of the Codex account a rollout's `session_meta` names: its
    /// `creator_account_id` and `creator_user_id` together (a Team workspace
    /// shares the account id); `None` where either is empty or overlong.
    #[must_use]
    pub fn codex(account_id: &str, user_id: &str) -> Option<AccountKey> {
        Self::hashed(b'x', "codex", &[account_id, user_id])
    }

    fn hashed(agent: u8, name: &str, id: &[&str]) -> Option<AccountKey> {
        if id.iter().any(|part| part.is_empty() || part.len() > ID_MAX) {
            return None;
        }
        let mut hash = aterm_digest::Sha256::new();
        hash.update(KEY_DOMAIN);
        hash.update([0]);
        hash.update(name);
        for part in id {
            hash.update([0]);
            hash.update(part);
        }
        let full = hash.finalize();
        let mut digest = [0; KEY_BYTES];
        digest.copy_from_slice(full.get(..KEY_BYTES)?);
        Some(AccountKey { agent, digest })
    }

    /// The key's first six hex digits, for a label made from it
    /// (`home-<key6>`, design §3.1).
    #[must_use]
    pub fn short(&self) -> String {
        let mut out = String::with_capacity(6);
        aterm_codec::hex::push_lower(&mut out, self.digest.get(..3).unwrap_or(&[]));
        out
    }
}

impl fmt::Display for AccountKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = String::with_capacity(2 + 2 * KEY_BYTES);
        out.push(char::from(self.agent));
        out.push('-');
        aterm_codec::hex::push_lower(&mut out, &self.digest);
        f.write_str(&out)
    }
}

impl fmt::Debug for AccountKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

// ---------------------------------------------------------------------------
// What is answered
// ---------------------------------------------------------------------------

/// A WORD FROM THE VENDOR'S FILE: kept only when it is 1 to [`WORD_MAX`]
/// bytes of printable ASCII, so nothing kept from the file can carry an
/// escape sequence to a terminal or run on past a line.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Word(Box<str>);

impl Word {
    /// `text` as a word, or `None`.
    #[must_use]
    pub fn new(text: &str) -> Option<Word> {
        let printable = text.bytes().all(|b| (b' '..=b'~').contains(&b));
        (printable && !text.is_empty() && text.len() <= WORD_MAX).then(|| Word(text.into()))
    }

    /// The word.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// THE PLAN, from `oauthAccount.organizationType` as Claude Code maps it
/// (`claude_max` → `max`, … — READ in 2.1.301), any other value
/// [`PlanWord::Other`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanWord {
    /// `claude_max`.
    Max,
    /// `claude_pro`.
    Pro,
    /// `claude_team`.
    Team,
    /// `claude_enterprise`.
    Enterprise,
    /// Any other organization type.
    Other,
}

impl PlanWord {
    fn of(organization_type: &str) -> PlanWord {
        match organization_type {
            "claude_max" => PlanWord::Max,
            "claude_pro" => PlanWord::Pro,
            "claude_team" => PlanWord::Team,
            "claude_enterprise" => PlanWord::Enterprise,
            _ => PlanWord::Other,
        }
    }

    /// The word JSON carries; for the four plans also the subscription
    /// word Claude Code compares with its flags.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            PlanWord::Max => "max",
            PlanWord::Pro => "pro",
            PlanWord::Team => "team",
            PlanWord::Enterprise => "enterprise",
            PlanWord::Other => "other",
        }
    }

    /// The word a person reads (`Max`).
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            PlanWord::Max => "Max",
            PlanWord::Pro => "Pro",
            PlanWord::Team => "Team",
            PlanWord::Enterprise => "Enterprise",
            PlanWord::Other => "other plan",
        }
    }
}

/// THE PLAN AND ITS TIERS (design §3.3), each `None` where the profile
/// does not say.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    /// [`PlanWord`].
    pub word: Option<PlanWord>,
    /// The served rate-limit tier — `userRateLimitTier`, else
    /// `organizationRateLimitTier` — with a trailing multiple taken as
    /// itself (`default_claude_max_5x` → `5x`), else the word as it is.
    pub tier: Option<Word>,
    /// `seatTier`.
    pub seat: Option<Word>,
    /// `billingType`.
    pub billing: Option<Word>,
    /// `planDisplayName`.
    pub display: Option<Word>,
    /// `subscriptionCreatedAt`, Unix seconds.
    pub subscribed_at: Option<i64>,
    /// `hasExtraUsageEnabled`.
    pub extra_usage: Option<bool>,
    /// `profileFetchedAt`, Unix seconds: when Claude Code last fetched the
    /// profile (it refetches one 24 h old at a start).
    pub fetched_at: Option<i64>,
}

impl Plan {
    /// The plan as a person reads it: its word and, where the tier is a
    /// multiple, the multiple (`Max 5x`, `Pro`); `None` with no plan word.
    #[must_use]
    pub fn title(&self) -> Option<String> {
        let word = self.word?.title();
        Some(
            match self.tier.as_ref().filter(|t| is_multiple(t.as_str())) {
                Some(tier) => format!("{word} {}", tier.as_str()),
                None => word.to_owned(),
            },
        )
    }
}

/// WHETHER USAGE CREDITS ARE ON, from `cachedExtraUsageDisabledReason`,
/// which Claude Code rewrites from every served reply's headers (design
/// F5): `null` when the header is absent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Credits {
    /// The key is absent, or holds neither text nor `null`.
    Unknown,
    /// `null`: credits are available.
    On,
    /// `out_of_credits`.
    Out,
    /// Any other reason; `None` for one that is not a [`Word`].
    Off(Option<Word>),
}

impl Credits {
    /// `on`, `out` or `off`; `None` for [`Credits::Unknown`].
    #[must_use]
    pub fn state(&self) -> Option<&'static str> {
        match self {
            Credits::Unknown => None,
            Credits::On => Some("on"),
            Credits::Out => Some("out"),
            Credits::Off(_) => Some("off"),
        }
    }

    /// The vendor's reason for [`Credits::Off`].
    #[must_use]
    pub fn reason(&self) -> Option<&Word> {
        match self {
            Credits::Off(reason) => reason.as_ref(),
            _ => None,
        }
    }
}

/// WHICH WINDOW A CACHED READING IS OF: aterm's closed bucket ids (design
/// §7.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bucket {
    /// `five_hour`.
    FiveHour,
    /// `seven_day`.
    SevenDay,
    /// `seven_day_sonnet`.
    SevenDaySonnet,
    /// `seven_day_opus`.
    SevenDayOpus,
    /// `seven_day_overage_included`: Fable's weekly bucket, every Fable
    /// name (`Fable`, `Fable 5`, …) — the body's `limits[]` row of kind
    /// `weekly_scoped` (design F2).
    Fable,
    /// `seven_day_other`: a `weekly_scoped` row of any other name.
    Other,
}

impl Bucket {
    /// The bucket's id.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Bucket::FiveHour => "five_hour",
            Bucket::SevenDay => "seven_day",
            Bucket::SevenDaySonnet => "seven_day_sonnet",
            Bucket::SevenDayOpus => "seven_day_opus",
            Bucket::Fable => "seven_day_overage_included",
            Bucket::Other => "seven_day_other",
        }
    }
}

/// ONE WINDOW AS THE CACHED `/usage` BODY HAD IT.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CachedWindow {
    /// [`Bucket`].
    pub bucket: Bucket,
    /// The row's model name, for a per-model weekly row.
    pub name: Option<Word>,
    /// The share used, in tenths of a percent (0..=1000).
    pub used_pct_x10: Option<u16>,
    /// When it resets, Unix seconds.
    pub resets_at: Option<i64>,
}

/// THE USAGE-CREDITS SPEND the body carries (`extra_usage`): amounts in the
/// currency's minor units (cents for USD), as the vendor sends them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CreditsMoney {
    /// `is_enabled`: whether credits can cover sends now.
    pub enabled: Option<bool>,
    /// `monthly_limit`.
    pub cap_minor: Option<i64>,
    /// `used_credits`.
    pub spent_minor: Option<i64>,
    /// `utilization`, in tenths of a percent.
    pub used_pct_x10: Option<u16>,
    /// `currency`.
    pub currency: Option<Word>,
    /// `disabled_reason`.
    pub disabled_reason: Option<Word>,
}

/// WHICH ONE-TIME CREDIT BLOCK, in aterm's words for the panel's titles
/// (the vendor's keys for them are codenames, which stay here).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OneTimeKind {
    /// The panel's "Claude Code and Cowork credit".
    CodeAndCowork,
    /// The panel's "Credit" block, which counts dollars.
    Credit,
}

impl OneTimeKind {
    /// The kind's word.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            OneTimeKind::CodeAndCowork => "code-and-cowork",
            OneTimeKind::Credit => "credit",
        }
    }
}

/// ONE ONE-TIME CREDIT BLOCK of the body.
#[derive(Clone, Debug, PartialEq)]
pub struct OneTime {
    /// [`OneTimeKind`].
    pub kind: OneTimeKind,
    /// The share used, in tenths of a percent.
    pub used_pct_x10: Option<u16>,
    /// When it resets or ends, Unix seconds.
    pub resets_at: Option<i64>,
    /// Dollars used.
    pub used_usd: Option<f64>,
    /// Dollars it holds.
    pub limit_usd: Option<f64>,
}

/// THE `/usage` BODY CLAUDE CODE CACHED (`cachedUsageUtilization`, design
/// F3): written when a person opens `/usage`, at most once a minute per
/// home. Kept only when it names the profile's own account.
#[derive(Clone, Debug, PartialEq)]
pub struct UsageCache {
    /// `fetchedAtMs`.
    pub fetched_at_ms: i64,
    /// The windows: the body's own, then its per-model weekly rows.
    pub windows: Vec<CachedWindow>,
    /// [`CreditsMoney`].
    pub credits: Option<CreditsMoney>,
    /// [`OneTime`].
    pub one_time: Vec<OneTime>,
}

/// HOW THE PLAN BILLS FABLE — a hint, in aterm's words (design §3.3, F18,
/// Q16), decided in the order Claude Code 2.1.301 decides it (READ in its
/// bundle): a login billed per request — an Enterprise seat
/// `enterprise_usage_based`, or the tier `default_claude_zero` — gates Fable
/// as it does the plan's own models, so included; else Enterprise on any
/// other seat, or a plan whose subscription word is in
/// `tengu_saffron_credits_only_tiers`, is credits only; else
/// `tengu_saffron_lattice` says whether a Fable model is billed to usage
/// credits (a `perModel` entry for a Fable id first; then `enabled: false`
/// says no, and `overageConsentRequired` or a `planLimitsEndDate` says yes),
/// with the date where it names one. A flag the answer needs that is absent,
/// or whose value or plan-wide keys are of another shape than Claude Code
/// parses, is [`FablePlan::Unknown`]: Claude Code would fall back to a
/// default there, and a hint does not guess which.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FablePlan {
    /// Fable counts against the plan's own limits.
    Included,
    /// Fable runs on usage credits only.
    CreditsOnly,
    /// The plan's Fable limits end at a date (`None`: a date that did not
    /// read), and Fable goes to usage credits.
    LimitsEnd(Option<i64>),
    /// A flag is absent or did not read (renamed, or another shape), or the
    /// plan is not known.
    Unknown,
}

impl FablePlan {
    /// `included`, `credits-only`, `limits-end` or `unknown`.
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            FablePlan::Included => "included",
            FablePlan::CreditsOnly => "credits-only",
            FablePlan::LimitsEnd(_) => "limits-end",
            FablePlan::Unknown => "unknown",
        }
    }

    /// The date the plan's Fable limits end.
    #[must_use]
    pub fn ends_at(&self) -> Option<i64> {
        match self {
            FablePlan::LimitsEnd(at) => *at,
            _ => None,
        }
    }
}

/// WHAT ONE `.claude.json` SAYS OF ITS ACCOUNT.
#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    /// The account's key; `None` with no `oauthAccount.accountUuid`.
    pub key: Option<AccountKey>,
    /// [`Plan`].
    pub plan: Plan,
    /// [`Credits`].
    pub credits: Credits,
    /// [`UsageCache`].
    pub usage: Option<UsageCache>,
    /// [`FablePlan`].
    pub fable: FablePlan,
    /// The model families a stored consent bills to usage credits with no
    /// dialog, lowercased (`fable` by `fableOverageConsentV2`, a family of
    /// `usageCreditsOnlyModelConsent`): any id's `true` counts — a home two
    /// logins share keeps both theirs.
    pub consented: Vec<String>,
    /// The model families billed to usage credits only, lowercased
    /// (`tengu_calm_seal`); `None`: the flag absent or of another shape.
    pub credits_only: Option<Vec<String>>,
}

/// WHY NO PROFILE WAS READ, one closed word each ([`NoProfile::word`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoProfile {
    /// The home could not be resolved, so no file was looked for: a
    /// relative `CLAUDE_CONFIG_DIR` with no `CLAUDE_PID` whose working
    /// directory it resolves under could be read, or no absolute `HOME`.
    NoHome,
    /// No file there.
    Missing,
    /// Something there that is not a regular file.
    NotAFile,
    /// Over [`PROFILE_MAX`].
    TooLarge,
    /// It could not be read, or is not JSON.
    Unreadable,
}

impl NoProfile {
    /// `no-home`, `no-profile`, `not-a-file`, `too-large` or `unreadable`.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            NoProfile::NoHome => "no-home",
            NoProfile::Missing => "no-profile",
            NoProfile::NotAFile => "not-a-file",
            NoProfile::TooLarge => "too-large",
            NoProfile::Unreadable => "unreadable",
        }
    }
}

// ---------------------------------------------------------------------------
// Where, and the read
// ---------------------------------------------------------------------------

/// THE PROFILE CLAUDE CODE KEEPS FOR THE HOME a process's environment names
/// — its raw `CLAUDE_CONFIG_DIR` (`config`, `None` unset) and its raw
/// `HOME` (`home`, `None` for the user's) — for a user whose home is
/// `user_home`, by the one resolver aterm names every Claude home with
/// (`Records::at`, `Records::profile`): `~/.claude.json` beside the user's
/// own `~/.claude` with `CLAUDE_CONFIG_DIR` unset, `<dir>/.claude.json`
/// inside the directory a set one names (`~/.claude` itself included), and
/// `$HOME`'s for an empty one. `cwd` reads the working directory a relative
/// one resolves under; `None` where there is none. The home's own profile,
/// which names the home; the file Claude Code reads for it is
/// [`effective`].
#[must_use]
pub fn path_of(
    user_home: &Path,
    config: Option<&str>,
    home: Option<&str>,
    cwd: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    Records::at(user_home, HomeVars { config, home }, cwd).profile(user_home)
}

/// The profile of the home whose records are in `dir`, the raw
/// `CLAUDE_CONFIG_DIR` that named it being `config` ([`path_of`], for a
/// caller that resolved the directory already).
#[must_use]
pub fn path_in(user_home: &Path, config: Option<&str>, dir: &Path) -> PathBuf {
    super::upgrade_drive::profile_in(user_home, HomeVars::config(config), dir)
}

/// THE FILE CLAUDE CODE READS AS THE PROFILE OF THE HOME whose own
/// profile is `plain` ([`path_of`]) and whose records are in `records`
/// (`CLAUDE_CONFIG_DIR ?? ~/.claude`), picked as Claude Code 2.1.301's
/// `v_()` picks it (READ in its bundle; a launch with its diskless option
/// takes the plain file): `<records>/.config.json` (the bundle's
/// `legacyPath`) when it is a regular file (a link followed) and either
/// holds a byte or, with a write bit set, stands where no plain file is;
/// else the plain file, which Claude Code names
/// `.claude-custom-oauth.json` when `CLAUDE_CODE_CUSTOM_OAUTH_URL` is set
/// and not empty (`custom_oauth`, [`custom_oauth`]: `R9()`). With
/// `records` unknown (an empty `CLAUDE_CONFIG_DIR` whose working
/// directory could not be read) no `.config.json` is looked for. `plain`
/// stays the home's identity; this names only the file that holds its
/// login.
#[must_use]
pub fn effective(plain: &Path, records: Option<&Path>, custom_oauth: bool) -> PathBuf {
    let plain = if custom_oauth {
        plain.with_file_name(".claude-custom-oauth.json")
    } else {
        plain.to_path_buf()
    };
    if let Some(legacy) = records.map(|dir| dir.join(".config.json"))
        && let Ok(meta) = std::fs::metadata(&legacy)
        && meta.is_file()
        && (meta.len() > 0 || (!meta.permissions().readonly() && !plain.exists()))
    {
        return legacy;
    }
    plain
}

/// The profile filename selected by a process's own exec environment.
/// Unknown context never grants a billing decision. No URL or credential is kept.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Selection {
    /// The exec environment was not read, or disagrees on the selector.
    #[default]
    Unknown,
    /// The ordinary `.claude.json` filename.
    Standard,
    /// The `.claude-custom-oauth.json` filename.
    CustomOauth,
}

impl Selection {
    /// Read only the selector from a captured exec environment. Empty means
    /// unread, not a known default. Conflicting duplicate values are unknown.
    #[must_use]
    pub fn of_exec_env(env: &[String]) -> Self {
        if env.is_empty() {
            return Self::Unknown;
        }
        let mut selected = None;
        for (_, value) in env
            .iter()
            .filter_map(|word| word.split_once('='))
            .filter(|(name, _)| *name == "CLAUDE_CODE_CUSTOM_OAUTH_URL")
        {
            let custom = !value.is_empty();
            if selected.is_some_and(|old| old != custom) {
                return Self::Unknown;
            }
            selected = Some(custom);
        }
        if selected.unwrap_or(false) {
            Self::CustomOauth
        } else {
            Self::Standard
        }
    }
}

/// The process's captured profile identity for a billing decision. This
/// live-only context is separate from older model/access-cache path identity.
/// It contains paths and a filename choice, never profile bytes or an OAuth URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Context {
    /// The exact plain profile path resolved from the process's HOME/config.
    pub plain: PathBuf,
    /// The process's resolved Claude record directory, for legacy selection.
    pub records: PathBuf,
    /// The filename choice from its own captured exec environment.
    pub selection: Selection,
    /// The captured HOME, USERPROFILE on Windows ([`super::accounts::HOMEDIR_ENV`];
    /// unset/empty remain distinct).
    pub home: Option<String>,
    /// The captured CLAUDE_CONFIG_DIR (unset/empty remain distinct).
    pub config: Option<String>,
    /// The captured stored-login selector (unset/empty remain distinct).
    pub login: Option<String>,
}

/// One exec variable, known unset versus unknown/conflicting. No environment
/// of the supervising process is consulted.
fn exec_var<'a>(env: &'a [String], name: &str) -> Option<Option<&'a str>> {
    if env.is_empty() {
        return None;
    }
    let mut found = None;
    for (_, value) in env
        .iter()
        .filter_map(|word| word.split_once('='))
        .filter(|(key, _)| *key == name)
    {
        if found.is_some_and(|old| old != value) {
            return None;
        }
        found = Some(value);
    }
    Some(found)
}

impl Context {
    /// The profile paths of already-resolved records, with the process's
    /// own HOME/config/login and filename selector. Pure captured data.
    pub(super) fn of_exec_env(user_home: &Path, records: &Path, env: &[String]) -> Option<Self> {
        let vars = HomeVars {
            config: exec_var(env, super::accounts::HOME_ENV)?,
            home: exec_var(env, super::accounts::HOMEDIR_ENV)?,
        };
        let selection = Selection::of_exec_env(env);
        if selection == Selection::Unknown {
            return None;
        }
        Some(Self {
            plain: super::upgrade_drive::profile_in(user_home, vars, records),
            records: records.to_path_buf(),
            selection,
            home: vars.home.map(str::to_owned),
            config: vars.config.map(str::to_owned),
            login: exec_var(env, super::accounts::LOGIN_ENV)?.map(str::to_owned),
        })
    }

    /// A Standard model move's complete profile environment. Each relevant
    /// variable is set or explicitly removed, so a shell's mutable inherited
    /// environment is never evidence of the target's billing context. The
    /// required home/login overlay remains authoritative; a changed context
    /// is refused by the caller. Custom OAuth needs its URL, which is never
    /// retained here: it cannot take this path with billing disabled.
    pub(super) fn for_launch(
        &self,
        user_home: &Path,
        cwd: &str,
        required: &[(&'static str, &str)],
    ) -> Option<LaunchEnv> {
        if self.selection != Selection::Standard {
            return None;
        }
        let mut values = Vec::new();
        let mut unset = Vec::new();
        for (name, captured) in [
            (super::accounts::HOMEDIR_ENV, self.home.as_deref()),
            (super::accounts::HOME_ENV, self.config.as_deref()),
            (super::accounts::LOGIN_ENV, self.login.as_deref()),
        ] {
            let value = required
                .iter()
                .find(|(key, _)| *key == name)
                .map_or(captured, |(_, value)| Some(*value));
            if let Some(value) = value {
                values.push((name, value.to_string()));
            } else {
                unset.push(name);
            }
        }
        if required.iter().any(|(name, _)| {
            *name != super::accounts::HOMEDIR_ENV
                && *name != super::accounts::HOME_ENV
                && *name != super::accounts::LOGIN_ENV
        }) {
            return None;
        }
        // The vendor's selector tests truthiness: an explicit empty value
        // selects Standard even if the shell currently holds a custom URL.
        values.push(("CLAUDE_CODE_CUSTOM_OAUTH_URL", String::new()));
        let env: Vec<String> = values
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        let vars = HomeVars {
            config: exec_var(&env, super::accounts::HOME_ENV)?,
            home: exec_var(&env, super::accounts::HOMEDIR_ENV)?,
        };
        let records = Records::at(user_home, vars, || {
            Path::new(cwd).is_absolute().then(|| PathBuf::from(cwd))
        });
        let context = Self::of_exec_env(user_home, records.dir()?, &env)?;
        Some(LaunchEnv {
            values,
            unset,
            context,
        })
    }
}

/// Required values and removals, plus the context resolved only from those
/// words. No shell environment, OAuth URL or profile bytes are retained.
pub(super) struct LaunchEnv {
    pub values: Vec<(&'static str, String)>,
    pub unset: Vec<&'static str>,
    pub context: Context,
}

/// The effective profile, with selection uncertainty preserved for a billing
/// decision. Unlike [`effective`]'s informational fallback, an unreadable
/// selector is an error; only `NotFound` proves a candidate absent.
/// A usable legacy file is also unknown here: the documented diskless
/// branch ignores it, and the captured context does not identify that branch.
/// This refuses a decision rather than merging two possible accounts.
///
/// # Errors
/// Missing process context or records, or selection metadata that cannot be read.
pub(super) fn effective_for_billing(
    plain: &Path,
    records: Option<&Path>,
    selection: Selection,
) -> std::io::Result<PathBuf> {
    let plain = match selection {
        Selection::Unknown => return Err(std::io::Error::other("profile selector unknown")),
        Selection::Standard => plain.to_path_buf(),
        Selection::CustomOauth => plain.with_file_name(".claude-custom-oauth.json"),
    };
    let records = records.ok_or_else(|| std::io::Error::other("records directory unknown"))?;
    let legacy = records.join(".config.json");
    let meta = match std::fs::metadata(&legacy) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(plain),
        Err(e) => return Err(e),
    };
    if !meta.is_file() || (meta.len() == 0 && meta.permissions().readonly()) {
        return Ok(plain);
    }
    if meta.len() > 0 {
        return Err(std::io::Error::other("legacy profile selection unknown"));
    }
    match std::fs::metadata(&plain) {
        Ok(_) => Ok(plain),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(std::io::Error::other("legacy profile selection unknown"))
        }
        Err(e) => Err(e),
    }
}

/// Whether a raw `CLAUDE_CODE_CUSTOM_OAUTH_URL` renames the profile
/// ([`effective`]): set and not empty, as JavaScript tests it.
#[must_use]
pub fn custom_oauth(raw: Option<&std::ffi::OsStr>) -> bool {
    raw.is_some_and(|url| !url.is_empty())
}

/// THE METHOD A CLAUDE CODE PROCESS'S LOGIN IS, for the one key the harness
/// presses on `/login`'s method picker (`supervise::policy::login`): the ONE
/// decision this module's record feeds. `env` is the process's own exec
/// environment (`KEY=value`), `user_home` the user's home; `renewable` says a
/// roster row with an email names the process's login (a claude.ai account:
/// a renewal's check holds it to `authMethod: claude.ai`).
///
/// A SUBSCRIPTION — the picker's `Claude account with subscription` — only
/// where the process authenticates by a stored login (nothing in its
/// environment gives it a key, a token, a provider or a route of its own:
/// [`super::accounts::ProcessLogin::of_exec_env`]) and that login is a
/// renewable row's, or the profile of its home names one of claude.ai's four
/// plans (`oauthAccount.organizationType`, [`PlanWord`]): a login Claude
/// Code's subscription sign-in made. `None` everywhere else — an environment
/// credential (`/login` cannot change it), a Console login, a profile that
/// cannot be read or names no plan: never a guess. The profile is the file
/// [`effective`] selects; unknown records (a relative or empty config directory
/// with no working directory) name no method. Reads only that allowlisted
/// profile ([`read`]) and its selection metadata.
#[must_use]
pub fn login_method(
    env: &[String],
    user_home: &Path,
    renewable: bool,
) -> Option<aterm_phase::LoginMethod> {
    let who = super::accounts::ProcessLogin::of_exec_env(env)?;
    if !who.stored_login {
        return None;
    }
    if renewable {
        return Some(aterm_phase::LoginMethod::Subscription);
    }
    let path = effective_of(env, user_home)?;
    let plan = read(&path).ok()?.plan.word?;
    (plan != PlanWord::Other).then_some(aterm_phase::LoginMethod::Subscription)
}

/// The profile Claude Code reads for the home a process's exec environment
/// `env` names ([`effective`] over [`path_of`]'s home), for a user whose
/// home is `user_home`; `None` where the records cannot be named (a
/// relative or empty config directory with no working directory).
fn effective_of(env: &[String], user_home: &Path) -> Option<PathBuf> {
    let var = |name: &str| {
        env.iter()
            .filter_map(|kv| kv.split_once('='))
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v)
    };
    let records = Records::at(
        user_home,
        HomeVars {
            config: var(super::accounts::HOME_ENV),
            home: var(super::accounts::HOMEDIR_ENV),
        },
        || None,
    );
    let plain = records.profile(user_home)?;
    Some(effective(
        &plain,
        Some(records.dir()?),
        custom_oauth(var("CLAUDE_CODE_CUSTOM_OAUTH_URL").map(std::ffi::OsStr::new)),
    ))
}

/// WHETHER THE LOGIN A CLAUDE CODE PROCESS'S HOME HOLDS ANSWERS TO `email`,
/// a roster row's address ([`super::accounts::email_matches`]): the identity
/// check of a login signed in again in its tab, as a renewal's check is of
/// its staging login — so a row is restored only for its own account
/// (`accounts::note_signed_back`; the review's acct-6). `env` is the
/// process's own exec environment, `user_home` the user's home; the profile
/// is the one [`effective`] selects. This login check reads
/// `oauthAccount.emailAddress` by a reader of that key alone:
/// compared in memory and its bytes zeroed, never returned, kept, logged or
/// printed. `None`: no profile read, or none that names an address.
#[must_use]
pub fn answers_to(env: &[String], user_home: &Path, email: &str) -> Option<bool> {
    let path = effective_of(env, user_home)?;
    read_with(&path, |bytes| {
        let Obj(raw) =
            aterm_json::from_slice::<Obj<RawWho>>(bytes).map_err(|_| NoProfile::Unreadable)?;
        let Some(Address(Some(address))) = raw.and_then(|who| who.oauth.0).map(|o| o.address)
        else {
            return Ok(None);
        };
        let answers = super::accounts::email_matches(email, &address);
        let mut kept = address.into_bytes();
        wipe(&mut kept);
        Ok(Some(answers))
    })
    .ok()
    .flatten()
}

/// Facts for the requested on-screen account/subscription display. These are volatile
/// presentation values, never account-stat records, settings or authentication material.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct SubscriptionDisplay {
    /// The email the selected profile actually names; never a configured roster guess.
    pub email: Option<String>,
    /// The selected account's subscription plan.
    pub plan: Option<String>,
    /// Its weekly quota, only while the selected account's cached sample is current.
    pub weekly: Option<String>,
}

/// Read the on-screen subscription facts from this process's selected profile. The
/// address, plan and account-validated usage come from the same bounded file read.
/// An environment credential route, unknown selector or unreadable profile answers nothing;
/// callers replace their previous display rather than retaining another account's facts.
/// The caller must also verify the process's settings and provider/profile-store sources
/// leave its stored login selected; this reader inspects the captured environment only.
/// Named secure-storage logins share their home's profile: its email must match the
/// roster binding for this exact login. The default login needs the same check when
/// its home lists several logins. A roster email gates the read, never supplies text.
/// `Some(empty)` means no logins are configured; `None` means the roster is unknown,
/// so the default shared home cannot establish which login the profile describes.
#[must_use]
pub fn subscription_for(
    env: &[String],
    user_home: &Path,
    records: &Path,
    roster: Option<&super::accounts::Roster>,
    now: i64,
) -> Option<SubscriptionDisplay> {
    let who = super::accounts::ProcessLogin::of_exec_env(env)?;
    if !who.stored_login {
        return None;
    }
    let context = Context::of_exec_env(user_home, records, env)?;
    let file = effective(
        &context.plain,
        Some(&context.records),
        context.selection == Selection::CustomOauth,
    );
    let display = read_with(&file, |bytes| parse_subscription(bytes, now)).ok()?;
    // CLAUDE_SECURESTORAGE_CONFIG_DIR changes the keychain item, not this file.
    // A different login can overwrite the shared profile while this tab stays open.
    let selected = (who.default_home || who.login.is_some())
        .then(|| {
            roster.and_then(|listed| {
                listed
                    .row_for_login(who.login.as_deref())
                    .or_else(|| listed.row_left(who.login.as_deref()))
            })
        })
        .flatten();
    if let Some(expected) = selected.and_then(|row| row.email.as_deref()) {
        if !display
            .email
            .as_deref()
            .is_some_and(|email| super::accounts::email_matches(expected, email))
        {
            return None;
        }
    } else if who.login.is_some()
        || (who.default_home && roster.is_none())
        || (who.default_home && roster.is_some_and(super::accounts::Roster::several))
    {
        return None;
    }
    Some(display)
}

fn parse_subscription(bytes: &[u8], now: i64) -> Result<SubscriptionDisplay, NoProfile> {
    let profile = parse(bytes)?;
    // This second typed allowlist reads only the address. The general Profile stays
    // unchanged, and unrelated values (including credential strings) remain skipped.
    let Obj(raw) =
        aterm_json::from_slice::<Obj<RawWho>>(bytes).map_err(|_| NoProfile::Unreadable)?;
    let email = raw
        .and_then(|who| who.oauth.0)
        .and_then(|oauth| oauth.address.0)
        .filter(|email| super::accounts::email_refusal(email).is_none());
    // The underlying parser discards cached usage with a different accountUuid.
    // Expired windows and a cache older than fifteen minutes cannot say what remains.
    let weekly = profile.usage.as_ref().and_then(|usage| {
        let observed = usage.fetched_at_ms.checked_div(1000)?;
        if observed > now || now.saturating_sub(observed) > 15 * 60 {
            return None;
        }
        let window = usage
            .windows
            .iter()
            .find(|row| row.bucket == Bucket::SevenDay)?;
        if window.resets_at.is_none_or(|reset| reset <= now) {
            return None;
        }
        let left = 1000_u16.saturating_sub(window.used_pct_x10?);
        Some(if left.is_multiple_of(10) {
            format!("weekly {}% left", left / 10)
        } else {
            format!("weekly {}.{}% left", left / 10, left % 10)
        })
    });
    let plan = profile
        .plan
        .display
        .as_ref()
        .map(|name| name.as_str().to_owned())
        .or_else(|| profile.plan.title());
    Ok(SubscriptionDisplay {
        email,
        plan,
        weekly,
    })
}

/// READ THE PROFILE AT `path` ([`parse`]). Opened without blocking (a FIFO
/// planted there cannot hold the reader), judged a regular file on the open
/// handle, read whole into one buffer sized once — never grown, so no copy
/// of it is left behind — and that buffer is zeroed before it is freed.
///
/// # Errors
///
/// [`NoProfile`]: why nothing was read.
pub fn read(path: &Path) -> Result<Profile, NoProfile> {
    read_with(path, parse)
}

/// [`read`]'s file handling, its bytes handed to `parse` before they are
/// zeroed.
fn read_with<T>(
    path: &Path,
    parse: impl FnOnce(&[u8]) -> Result<T, NoProfile>,
) -> Result<T, NoProfile> {
    let mut file = match open_nonblocking(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(NoProfile::Missing),
        // Windows refuses to open a directory as a file at all (access
        // denied): what stands there is told by its metadata, nothing read.
        Err(_) if std::fs::metadata(path).is_ok_and(|meta| !meta.is_file()) => {
            return Err(NoProfile::NotAFile);
        }
        Err(_) => return Err(NoProfile::Unreadable),
    };
    let meta = file.metadata().map_err(|_| NoProfile::Unreadable)?;
    if !meta.is_file() {
        return Err(NoProfile::NotAFile);
    }
    if meta.len() > PROFILE_MAX {
        return Err(NoProfile::TooLarge);
    }
    // One byte past the length the handle reported: a file that grew while
    // it was read fills it, and reads as unreadable rather than as a cut.
    let size = usize::try_from(meta.len()).map_err(|_| NoProfile::TooLarge)?;
    let mut buf = vec![0_u8; size.saturating_add(1)];
    let mut filled = 0;
    let read = loop {
        let Some(rest) = buf.get_mut(filled..) else {
            break Err(NoProfile::Unreadable);
        };
        if rest.is_empty() {
            break Err(NoProfile::Unreadable);
        }
        match file.read(rest) {
            Ok(0) => break Ok(filled),
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break Err(NoProfile::Unreadable),
        }
    };
    let answer = read.and_then(|n| parse(buf.get(..n).unwrap_or(&[])));
    wipe(&mut buf);
    answer
}

/// Zero `buf`, and keep the compiler from dropping the writes as dead
/// before the free.
fn wipe(buf: &mut [u8]) {
    buf.fill(0);
    std::hint::black_box(&*buf);
}

/// Open `path` for reading without blocking ([`read`]).
fn open_nonblocking(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NONBLOCK);
    }
    options.open(path)
}

/// READ A PROFILE FROM ITS BYTES: the allowlist and nothing else (the
/// module docs say how).
///
/// # Errors
///
/// [`NoProfile::Unreadable`] for bytes that are not one JSON object.
pub fn parse(bytes: &[u8]) -> Result<Profile, NoProfile> {
    let Obj(raw) =
        aterm_json::from_slice::<Obj<RawFile>>(bytes).map_err(|_| NoProfile::Unreadable)?;
    let raw = raw.ok_or(NoProfile::Unreadable)?;
    Ok(profile_of(raw))
}

fn profile_of(raw: RawFile) -> Profile {
    let oauth = raw.oauth.0.unwrap_or_default();
    let key = oauth.key.0;
    let plan = Plan {
        word: oauth
            .organization_type
            .word()
            .map(|w| PlanWord::of(w.as_str())),
        tier: oauth
            .user_tier
            .word()
            .or_else(|| oauth.organization_tier.word())
            .map(tier_word),
        seat: oauth.seat.word(),
        billing: oauth.billing.word(),
        display: oauth.display.word(),
        subscribed_at: oauth.subscribed.word().and_then(|w| instant(w.as_str())),
        extra_usage: oauth.extra_usage.flag(),
        fetched_at: oauth.fetched.num().and_then(ms_to_s),
    };
    let credits = match raw.credits {
        Leaf::Null => Credits::On,
        Leaf::Text(Some(reason)) if reason.as_str() == "out_of_credits" => Credits::Out,
        Leaf::Text(reason) => Credits::Off(reason),
        _ => Credits::Unknown,
    };
    let usage = raw
        .usage
        .0
        .filter(|cache| key.is_some() && cache.key.0 == key)
        .and_then(usage_of);
    let features = raw.features.0.unwrap_or_default();
    let credits_only = features
        .calm_seal
        .0
        .as_ref()
        .map(|words| words.iter().filter_map(|w| family(w.as_str())).collect());
    let fable = fable_plan(&plan, features);
    let mut consented: Vec<String> = raw.model_consent.0;
    if raw.fable_consent.0 && !consented.iter().any(|f| f == "fable") {
        consented.insert(0, "fable".to_string());
    }
    Profile {
        key,
        plan,
        credits,
        usage,
        fable,
        consented,
        credits_only,
    }
}

/// A rate-limit tier's word: a trailing `_<digits>x` taken as the multiple
/// (`default_claude_max_20x` → `20x`), else the word itself.
fn tier_word(raw: Word) -> Word {
    let multiple = raw
        .as_str()
        .rsplit_once('_')
        .map(|(_, tail)| tail)
        .filter(|tail| is_multiple(tail));
    multiple.and_then(Word::new).unwrap_or(raw)
}

/// Whether `tier` is a multiple of the plan's base usage (`5x`, `20x`).
fn is_multiple(tier: &str) -> bool {
    tier.strip_suffix('x')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

fn usage_of(cache: RawCache) -> Option<UsageCache> {
    let fetched_at_ms = cache.fetched_ms.num().and_then(whole)?;
    let body = cache.utilization.0?;
    let mut windows = Vec::new();
    for (bucket, window) in [
        (Bucket::FiveHour, body.five_hour),
        (Bucket::SevenDay, body.seven_day),
        (Bucket::SevenDaySonnet, body.seven_day_sonnet),
        (Bucket::SevenDayOpus, body.seven_day_opus),
    ] {
        if let Some(window) = window.0 {
            windows.push(CachedWindow {
                bucket,
                name: None,
                used_pct_x10: window.utilization.num().and_then(pct_x10),
                resets_at: window.resets_at.word().and_then(|w| instant(w.as_str())),
            });
        }
    }
    for row in body.limits.0 {
        if row
            .kind
            .word()
            .is_none_or(|kind| kind.as_str() != "weekly_scoped")
        {
            continue;
        }
        let Some(name) = row
            .scope
            .0
            .and_then(|scope| scope.model.0)
            .and_then(|m| m.display_name.word())
        else {
            continue;
        };
        let fable = name.as_str() == "Fable" || name.as_str().starts_with("Fable ");
        windows.push(CachedWindow {
            bucket: if fable { Bucket::Fable } else { Bucket::Other },
            name: Some(name),
            used_pct_x10: row.percent.num().and_then(pct_x10),
            resets_at: row.resets_at.word().and_then(|w| instant(w.as_str())),
        });
    }
    let credits = body.extra_usage.0.map(|extra| CreditsMoney {
        enabled: extra.is_enabled.flag(),
        cap_minor: extra.monthly_limit.num().and_then(whole),
        spent_minor: extra.used_credits.num().and_then(whole),
        used_pct_x10: extra.utilization.num().and_then(pct_x10),
        currency: extra.currency.word(),
        disabled_reason: extra.disabled_reason.word(),
    });
    let mut one_time = Vec::new();
    if let Some(block) = body.cinder_cove.0 {
        one_time.push(OneTime {
            kind: OneTimeKind::CodeAndCowork,
            used_pct_x10: block.utilization.num().and_then(pct_x10),
            resets_at: block.resets_at.word().and_then(|w| instant(w.as_str())),
            used_usd: None,
            limit_usd: None,
        });
    }
    if let Some(block) = body.wattle_ember.0 {
        one_time.push(OneTime {
            kind: OneTimeKind::Credit,
            used_pct_x10: block.utilization.num().and_then(pct_x10),
            resets_at: block.resets_at.word().and_then(|w| instant(w.as_str())),
            used_usd: block
                .used_dollars
                .num()
                .filter(|d| d.is_finite() && *d >= 0.0),
            limit_usd: block
                .limit_dollars
                .num()
                .filter(|d| d.is_finite() && *d >= 0.0),
        });
    }
    Some(UsageCache {
        fetched_at_ms,
        windows,
        credits,
        one_time,
    })
}

/// [`FablePlan`], from the plan and the two flags, in Claude Code's order.
fn fable_plan(plan: &Plan, flags: RawFeatures) -> FablePlan {
    let Some(word) = plan.word.filter(|word| *word != PlanWord::Other) else {
        return FablePlan::Unknown;
    };
    let is = |field: &Option<Word>, text: &str| field.as_ref().is_some_and(|w| w.as_str() == text);
    let usage_based = word == PlanWord::Enterprise && is(&plan.seat, "enterprise_usage_based");
    if usage_based || is(&plan.tier, "default_claude_zero") {
        return FablePlan::Included;
    }
    if word == PlanWord::Enterprise {
        return FablePlan::CreditsOnly;
    }
    let Words(Some(tiers)) = flags.tiers else {
        return FablePlan::Unknown;
    };
    if tiers.iter().any(|tier| tier.as_str() == word.word()) {
        return FablePlan::CreditsOnly;
    }
    let Obj(Some(lattice)) = flags.lattice else {
        return FablePlan::Unknown;
    };
    // The plan-wide keys as Claude Code's parse admits them: absent, or a
    // boolean (a string for the date). Any other shape fails its parse.
    let flag = |leaf: &Leaf| match leaf {
        Leaf::Absent => Some(None),
        Leaf::Flag(b) => Some(Some(*b)),
        _ => None,
    };
    let ends = match lattice.ends {
        Leaf::Absent => None,
        Leaf::Text(date) => Some(date.and_then(|d| instant(d.as_str()))),
        _ => return FablePlan::Unknown,
    };
    let (Some(enabled), Some(consent)) = (flag(&lattice.enabled), flag(&lattice.consent)) else {
        return FablePlan::Unknown;
    };
    let billed = lattice
        .per_model
        .0
        .unwrap_or_else(|| enabled != Some(false) && (consent == Some(true) || ends.is_some()));
    match (billed, ends) {
        (false, _) => FablePlan::Included,
        (true, Some(at)) => FablePlan::LimitsEnd(at),
        (true, None) => FablePlan::CreditsOnly,
    }
}

/// A share in percent (0-100, as the body carries it) in tenths, capped at
/// 100%; `None` for a negative or non-finite one.
fn pct_x10(pct: f64) -> Option<u16> {
    if !pct.is_finite() || pct < 0.0 {
        return None;
    }
    // Bounded to 0..=1000 by the `min`, so the cast keeps every value.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "0..=1000"
    )]
    Some((pct * 10.0).round().min(1000.0) as u16)
}

/// A whole number the file wrote as a JSON number; `None` past `i64` or not
/// finite.
fn whole(n: f64) -> Option<i64> {
    // Bounded by the range check, so the cast keeps every value.
    #[expect(clippy::cast_possible_truncation, reason = "range checked")]
    (n.is_finite() && n.abs() < 9.0e15).then(|| n.round() as i64)
}

/// Milliseconds since the epoch, in seconds.
fn ms_to_s(ms: f64) -> Option<i64> {
    whole(ms).map(|ms| ms.div_euclid(1000))
}

/// AN INSTANT AS THE VENDOR WRITES ONE: RFC 3339 with or without a fraction,
/// in UTC (`Z`) or at an offset (`+00:00`, applied), or a bare date
/// (`YYYY-MM-DD`, its midnight UTC). Unix seconds.
#[must_use]
pub fn instant(text: &str) -> Option<i64> {
    use aterm_types::rfc3339::{parse_utc, parse_utc_fractional};
    if text.len() == 10 {
        return parse_utc(&format!("{text}T00:00:00Z"));
    }
    if text.ends_with('Z') {
        return parse_utc_fractional(text);
    }
    let split = text.len().checked_sub(6)?;
    let (head, zone) = (text.get(..split)?, text.get(split..)?.as_bytes());
    let sign = match zone {
        [b'+', _, _, b':', _, _] => 1,
        [b'-', _, _, b':', _, _] => -1,
        _ => return None,
    };
    let digits = |pair: &[u8]| -> Option<i64> {
        let [tens, ones] = pair else { return None };
        (tens.is_ascii_digit() && ones.is_ascii_digit())
            .then(|| i64::from(tens - b'0') * 10 + i64::from(ones - b'0'))
    };
    let (hours, minutes) = (digits(zone.get(1..3)?)?, digits(zone.get(4..6)?)?);
    if hours > 23 || minutes > 59 {
        return None;
    }
    let at = parse_utc_fractional(&format!("{head}Z"))?;
    Some(at - sign * (hours * 3600 + minutes * 60))
}

// ---------------------------------------------------------------------------
// The typed reader: the allowlist's shapes, every leaf lenient
// ---------------------------------------------------------------------------

/// A MODEL FAMILY'S NAME as a key or a flag spells it (`Fable`, `nimbus`),
/// lowercased: letters alone, at most 32 — an id (digits, hyphens) is none.
fn family(name: &str) -> Option<String> {
    (!name.is_empty() && name.len() <= 32 && name.bytes().all(|b| b.is_ascii_alphabetic()))
        .then(|| name.to_ascii_lowercase())
}

/// The file: the six keys read; every other key skipped.
#[derive(Default, Deserialize)]
#[serde(default)]
struct RawFile {
    #[serde(rename = "fableOverageConsentV2")]
    fable_consent: Consent,
    #[serde(rename = "usageCreditsOnlyModelConsent")]
    model_consent: Consents,
    #[serde(rename = "oauthAccount")]
    oauth: Obj<RawOauth>,
    #[serde(rename = "cachedExtraUsageDisabledReason")]
    credits: Leaf,
    #[serde(rename = "cachedUsageUtilization")]
    usage: Obj<RawCache>,
    #[serde(rename = "cachedGrowthBookFeatures")]
    features: Obj<RawFeatures>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawOauth {
    #[serde(rename = "accountUuid")]
    key: Hashed,
    #[serde(rename = "organizationType")]
    organization_type: Leaf,
    #[serde(rename = "userRateLimitTier")]
    user_tier: Leaf,
    #[serde(rename = "organizationRateLimitTier")]
    organization_tier: Leaf,
    #[serde(rename = "seatTier")]
    seat: Leaf,
    #[serde(rename = "billingType")]
    billing: Leaf,
    #[serde(rename = "subscriptionCreatedAt")]
    subscribed: Leaf,
    #[serde(rename = "hasExtraUsageEnabled")]
    extra_usage: Leaf,
    #[serde(rename = "planDisplayName")]
    display: Leaf,
    #[serde(rename = "profileFetchedAt")]
    fetched: Leaf,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawCache {
    #[serde(rename = "fetchedAtMs")]
    fetched_ms: Leaf,
    #[serde(rename = "accountUuid")]
    key: Hashed,
    utilization: Obj<RawBody>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawBody {
    five_hour: Obj<RawWindow>,
    seven_day: Obj<RawWindow>,
    seven_day_sonnet: Obj<RawWindow>,
    seven_day_opus: Obj<RawWindow>,
    limits: List<RawLimit>,
    extra_usage: Obj<RawExtra>,
    cinder_cove: Obj<RawWindow>,
    wattle_ember: Obj<RawGrant>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawWindow {
    utilization: Leaf,
    resets_at: Leaf,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawLimit {
    kind: Leaf,
    percent: Leaf,
    resets_at: Leaf,
    scope: Obj<RawScope>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawScope {
    model: Obj<RawNamed>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawNamed {
    display_name: Leaf,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawExtra {
    is_enabled: Leaf,
    monthly_limit: Leaf,
    used_credits: Leaf,
    utilization: Leaf,
    currency: Leaf,
    disabled_reason: Leaf,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawGrant {
    utilization: Leaf,
    resets_at: Leaf,
    limit_dollars: Leaf,
    used_dollars: Leaf,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawFeatures {
    #[serde(rename = "tengu_saffron_credits_only_tiers")]
    tiers: Words,
    #[serde(rename = "tengu_saffron_lattice")]
    lattice: Obj<RawLattice>,
    #[serde(rename = "tengu_calm_seal")]
    calm_seal: Words,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawLattice {
    enabled: Leaf,
    #[serde(rename = "planLimitsEndDate")]
    ends: Leaf,
    #[serde(rename = "overageConsentRequired")]
    consent: Leaf,
    #[serde(rename = "perModel")]
    per_model: FableEntries,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawEnabled {
    enabled: Leaf,
}

/// Drain one container's values unread, for a visitor handed a shape it
/// does not keep.
fn drain_seq<'de, A: SeqAccess<'de>>(mut seq: A) -> Result<(), A::Error> {
    while seq.next_element::<IgnoredAny>()?.is_some() {}
    Ok(())
}

fn drain_map<'de, A: MapAccess<'de>>(mut map: A) -> Result<(), A::Error> {
    while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
    Ok(())
}

/// The scalar visitor methods of a reader that keeps no scalar: each
/// answers `$none`.
macro_rules! lenient_scalars {
    ($none:expr) => {
        fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
            Ok($none)
        }
        fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
            Ok($none)
        }
        fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
            Ok($none)
        }
        fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
            Ok($none)
        }
        fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
            Ok($none)
        }
        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok($none)
        }
        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok($none)
        }
    };
}

/// The email-only allowlist shared by the login check and volatile footer display.
#[derive(Default, Deserialize)]
#[serde(default)]
struct RawWho {
    #[serde(rename = "oauthAccount")]
    oauth: Obj<RawAddress>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawAddress {
    #[serde(rename = "emailAddress")]
    address: Address,
}

/// An address: a string, else nothing (any other value read past, unkept).
#[derive(Default)]
struct Address(Option<String>);

impl<'de> Deserialize<'de> for Address {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Address;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any value")
            }
            fn visit_str<E: de::Error>(self, s: &str) -> Result<Address, E> {
                Ok(Address(Some(s.to_owned())))
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Address, E> {
                Ok(Address(None))
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Address, E> {
                Ok(Address(None))
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Address, E> {
                Ok(Address(None))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Address, E> {
                Ok(Address(None))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Address, E> {
                Ok(Address(None))
            }
            fn visit_none<E: de::Error>(self) -> Result<Address, E> {
                Ok(Address(None))
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Address, A::Error> {
                drain_map(map).map(|()| Address(None))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Address, A::Error> {
                drain_seq(seq).map(|()| Address(None))
            }
        }
        d.deserialize_any(V)
    }
}

/// AN OBJECT OF THE ALLOWLIST: `T` where the value is an object, `None`
/// for any other value (read past, unkept). `T`'s own keys are the only
/// ones read.
struct Obj<T>(Option<T>);

impl<T> Default for Obj<T> {
    fn default() -> Self {
        Obj(None)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Obj<T> {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<T>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for V<T> {
            type Value = Obj<T>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any value")
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Obj<T>, A::Error> {
                T::deserialize(de::value::MapAccessDeserializer::new(map)).map(|t| Obj(Some(t)))
            }
            lenient_scalars!(Obj(None));
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Obj<T>, A::Error> {
                drain_seq(seq).map(|()| Obj(None))
            }
        }
        d.deserialize_any(V(PhantomData))
    }
}

/// AN ARRAY OF ALLOWLIST OBJECTS: each object element read as `T`, every
/// other element read past; empty for any value that is not an array.
struct List<T>(Vec<T>);

impl<T> Default for List<T> {
    fn default() -> Self {
        List(Vec::new())
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for List<T> {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<T>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for V<T> {
            type Value = List<T>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any value")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<List<T>, A::Error> {
                let mut out = Vec::new();
                while let Some(Obj(item)) = seq.next_element::<Obj<T>>()? {
                    out.extend(item);
                }
                Ok(List(out))
            }
            lenient_scalars!(List(Vec::new()));
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<List<T>, A::Error> {
                drain_map(map).map(|()| List(Vec::new()))
            }
        }
        d.deserialize_any(V(PhantomData))
    }
}

/// ONE SCALAR OF THE ALLOWLIST, whatever its type turned out to be: an
/// array or an object where a scalar was expected is read past, unkept.
#[derive(Default)]
enum Leaf {
    /// The key was not there.
    #[default]
    Absent,
    /// `null`.
    Null,
    /// A string: [`Word`], or `None` for one that is not a word.
    Text(Option<Word>),
    /// A number.
    Num(f64),
    /// A boolean.
    Flag(bool),
    /// An array or an object.
    Other,
}

impl Leaf {
    fn word(&self) -> Option<Word> {
        match self {
            Leaf::Text(word) => word.clone(),
            _ => None,
        }
    }

    fn num(&self) -> Option<f64> {
        match self {
            Leaf::Num(n) => Some(*n),
            _ => None,
        }
    }

    fn flag(&self) -> Option<bool> {
        match self {
            Leaf::Flag(b) => Some(*b),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for Leaf {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Leaf;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any value")
            }
            fn visit_bool<E: de::Error>(self, b: bool) -> Result<Leaf, E> {
                Ok(Leaf::Flag(b))
            }
            #[expect(clippy::cast_precision_loss, reason = "a figure, not an id")]
            fn visit_i64<E: de::Error>(self, n: i64) -> Result<Leaf, E> {
                Ok(Leaf::Num(n as f64))
            }
            #[expect(clippy::cast_precision_loss, reason = "a figure, not an id")]
            fn visit_u64<E: de::Error>(self, n: u64) -> Result<Leaf, E> {
                Ok(Leaf::Num(n as f64))
            }
            fn visit_f64<E: de::Error>(self, n: f64) -> Result<Leaf, E> {
                Ok(Leaf::Num(n))
            }
            fn visit_str<E: de::Error>(self, s: &str) -> Result<Leaf, E> {
                Ok(Leaf::Text(Word::new(s)))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Leaf, E> {
                Ok(Leaf::Null)
            }
            fn visit_none<E: de::Error>(self) -> Result<Leaf, E> {
                Ok(Leaf::Null)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Leaf, A::Error> {
                drain_seq(seq).map(|()| Leaf::Other)
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Leaf, A::Error> {
                drain_map(map).map(|()| Leaf::Other)
            }
        }
        d.deserialize_any(V)
    }
}

/// A CLAUDE ACCOUNT ID, HASHED AS IT IS READ ([`AccountKey::claude`]): the
/// parser hands the string over borrowed from the file's bytes, and only
/// the key is kept.
#[derive(Default)]
struct Hashed(Option<AccountKey>);

impl<'de> Deserialize<'de> for Hashed {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Hashed;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any value")
            }
            fn visit_str<E: de::Error>(self, s: &str) -> Result<Hashed, E> {
                Ok(Hashed(AccountKey::claude(s)))
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Hashed, E> {
                Ok(Hashed(None))
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Hashed, E> {
                Ok(Hashed(None))
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Hashed, E> {
                Ok(Hashed(None))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Hashed, E> {
                Ok(Hashed(None))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Hashed, E> {
                Ok(Hashed(None))
            }
            fn visit_none<E: de::Error>(self) -> Result<Hashed, E> {
                Ok(Hashed(None))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Hashed, A::Error> {
                drain_seq(seq).map(|()| Hashed(None))
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Hashed, A::Error> {
                drain_map(map).map(|()| Hashed(None))
            }
        }
        d.deserialize_any(V)
    }
}

/// AN ARRAY OF WORDS, `None` unless the value is an array of strings that
/// are all words (Claude Code reads anything else as unparseable).
#[derive(Default)]
struct Words(Option<Vec<Word>>);

impl<'de> Deserialize<'de> for Words {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Words;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any value")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Words, A::Error> {
                let mut out = Some(Vec::new());
                while let Some(leaf) = seq.next_element::<Leaf>()? {
                    match (leaf, out.as_mut()) {
                        (Leaf::Text(Some(word)), Some(words)) => words.push(word),
                        _ => out = None,
                    }
                }
                Ok(Words(out))
            }
            lenient_scalars!(Words(None));
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Words, A::Error> {
                drain_map(map).map(|()| Words(None))
            }
        }
        d.deserialize_any(V)
    }
}

/// `tengu_saffron_lattice.perModel`, as the one answer this reader needs:
/// whether a FABLE model is billed to usage credits — `Some(true)` where any
/// entry for an id that names Fable says `enabled: true` (Claude Code's own
/// rule where two spellings of a model disagree), `Some(false)` where every
/// such entry says `false`, `None` with no such entry or a value that is
/// not an object. The ids are read for that and not kept.
#[derive(Default)]
struct FableEntries(Option<bool>);

impl<'de> Deserialize<'de> for FableEntries {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = FableEntries;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any value")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<FableEntries, A::Error> {
                let mut billed = None;
                while let Some(IsFable(fable)) = map.next_key::<IsFable>()? {
                    let Obj(entry) = map.next_value::<Obj<RawEnabled>>()?;
                    let Some(enabled) = entry.and_then(|e| e.enabled.flag()) else {
                        continue;
                    };
                    if fable {
                        billed = Some(billed.unwrap_or(false) || enabled);
                    }
                }
                Ok(FableEntries(billed))
            }
            lenient_scalars!(FableEntries(None));
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<FableEntries, A::Error> {
                drain_seq(seq).map(|()| FableEntries(None))
            }
        }
        d.deserialize_any(V)
    }
}

/// A STORED CONSENT (`{ "<org or acct:id>": true }`): whether any entry
/// holds `true`. The keys — an organization's or an account's id — are read
/// past by the parser's ignored-value path, never built.
#[derive(Default)]
struct Consent(bool);

impl<'de> Deserialize<'de> for Consent {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Consent;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any value")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Consent, A::Error> {
                let mut given = false;
                while map.next_key::<IgnoredAny>()?.is_some() {
                    given |= matches!(map.next_value::<Leaf>()?, Leaf::Flag(true));
                }
                Ok(Consent(given))
            }
            lenient_scalars!(Consent(false));
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Consent, A::Error> {
                drain_seq(seq).map(|()| Consent(false))
            }
        }
        d.deserialize_any(V)
    }
}

/// THE STORED CONSENTS PER FAMILY (`{ "<family>": { "<org or acct:id>":
/// true } }`): the families ([`family`]) whose [`Consent`] holds. A key that
/// names no family is read past, unkept.
#[derive(Default)]
struct Consents(Vec<String>);

impl<'de> Deserialize<'de> for Consents {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Consents;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any value")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Consents, A::Error> {
                let mut families = Vec::new();
                while let Some(Family(name)) = map.next_key::<Family>()? {
                    let Consent(given) = map.next_value::<Consent>()?;
                    if let Some(name) = name.filter(|_| given)
                        && !families.contains(&name)
                    {
                        families.push(name);
                    }
                }
                Ok(Consents(families))
            }
            lenient_scalars!(Consents(Vec::new()));
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Consents, A::Error> {
                drain_seq(seq).map(|()| Consents(Vec::new()))
            }
        }
        d.deserialize_any(V)
    }
}

/// An object key, kept only where it names a model family ([`family`]).
struct Family(Option<String>);

impl<'de> Deserialize<'de> for Family {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = Family;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a key")
            }
            fn visit_str<E: de::Error>(self, s: &str) -> Result<Family, E> {
                Ok(Family(family(s)))
            }
        }
        d.deserialize_str(V)
    }
}

/// An object key, read for whether it names a Fable model and not kept.
struct IsFable(bool);

impl<'de> Deserialize<'de> for IsFable {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = IsFable;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a key")
            }
            fn visit_str<E: de::Error>(self, s: &str) -> Result<IsFable, E> {
                let fable = s
                    .as_bytes()
                    .windows(5)
                    .any(|w| w.eq_ignore_ascii_case(b"fable"));
                Ok(IsFable(fable))
            }
        }
        d.deserialize_str(V)
    }
}

#[path = "profile_tests.rs"]
#[cfg(test)]
mod tests;
