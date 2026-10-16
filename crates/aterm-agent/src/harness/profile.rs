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
//!   `emailAddress`, `displayName`, `fullName`, `organizationName` or
//!   `organizationUuid`.
//! * `cachedExtraUsageDisabledReason`: [`Credits`].
//! * `cachedUsageUtilization`: `fetchedAtMs`, `accountUuid` (hashed, and the
//!   body is dropped unless it names the profile's own account, as Claude
//!   Code drops it) and the usage body's windows, its per-model weekly rows,
//!   its usage-credits spend and its two one-time credit blocks
//!   ([`UsageCache`]).
//! * `cachedGrowthBookFeatures`: the two Fable billing flags by name,
//!   `tengu_saffron_credits_only_tiers` and `tengu_saffron_lattice`
//!   ([`FablePlan`]). Their names stay in this file: what leaves it is
//!   aterm's own word.
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
//! reads it.

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

/// Whether a raw `CLAUDE_CODE_CUSTOM_OAUTH_URL` renames the profile
/// ([`effective`]): set and not empty, as JavaScript tests it.
#[must_use]
pub fn custom_oauth(raw: Option<&std::ffi::OsStr>) -> bool {
    raw.is_some_and(|url| !url.is_empty())
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
    let mut file = match open_nonblocking(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(NoProfile::Missing),
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
    let fable = fable_plan(&plan, raw.features.0.unwrap_or_default());
    Profile {
        key,
        plan,
        credits,
        usage,
        fable,
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

/// The file: the four keys read; every other key skipped.
#[derive(Default, Deserialize)]
#[serde(default)]
struct RawFile {
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
