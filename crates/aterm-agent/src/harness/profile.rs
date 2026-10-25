// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claude Code's profile, read by an allowlist.

use std::fmt;
use std::io::Read as _;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde::de::{self, IgnoredAny, MapAccess, SeqAccess, Visitor};

use super::upgrade_drive::{HomeVars, Records};

/// The most of a `.claude.json` that is read.
pub const PROFILE_MAX: u64 = 32 * 1024 * 1024;

/// The longest text kept from the file ([`Word`]).
pub const WORD_MAX: usize = 64;

/// The longest id hashed into an [`AccountKey`].
const ID_MAX: usize = 128;

const KEY_DOMAIN: &[u8] = b"aterm-account-v1";

const KEY_BYTES: usize = 6;

/// One account's key: the same on every machine and holding no secret (design §3.1).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AccountKey {
    /// `c` for Claude Code, `x` for Codex, `g` for Grok Build, `k` for Kimi Code.
    agent: u8,
    /// The digest's first [`KEY_BYTES`].
    digest: [u8; KEY_BYTES],
}

impl AccountKey {
    /// The key of the Claude Code account whose `accountUuid` is `id`.
    #[must_use]
    pub fn claude(id: &str) -> Option<AccountKey> {
        Self::hashed(b'c', "claude", &[id])
    }

    /// The key of the Codex account a rollout's `session_meta` names.
    #[must_use]
    pub fn codex(account_id: &str, user_id: &str) -> Option<AccountKey> {
        Self::hashed(b'x', "codex", &[account_id, user_id])
    }

    /// The key of the Grok Build account whose xAI user id is `user_id`.
    #[must_use]
    pub fn grok(user_id: &str) -> Option<AccountKey> {
        Self::hashed(b'g', "grok", &[user_id])
    }

    /// The key of the Kimi Code account whose user id is `user_id` on `region`'s account system:
    /// the two regions are separate systems, so one id on each is two accounts.
    #[must_use]
    pub fn kimi(region: crate::ai::connector::Region, user_id: &str) -> Option<AccountKey> {
        Self::hashed(b'k', "kimi", &[region.as_str(), user_id])
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

    /// The key's first six hex digits, for a label made from it (`home-<key6>`, design §3.1).
    #[must_use]
    pub fn short(&self) -> String {
        let mut out = String::with_capacity(6);
        aterm_codec::hex::push_lower(&mut out, self.digest.get(..3).unwrap_or(&[]));
        out
    }

    /// A key back from its text ([`fmt::Display`]).
    #[must_use]
    pub fn parse(text: &str) -> Option<AccountKey> {
        let (agent, hex) = match text.split_once('-')? {
            ("c", hex) => (b'c', hex),
            ("x", hex) => (b'x', hex),
            ("g", hex) => (b'g', hex),
            ("k", hex) => (b'k', hex),
            _ => return None,
        };
        if !aterm_codec::hex::is_lower(hex, 2 * KEY_BYTES) {
            return None;
        }
        let mut digest = [0; KEY_BYTES];
        aterm_codec::hex::decode_into(hex.as_bytes(), &mut digest)
            .then_some(AccountKey { agent, digest })
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

/// A word from the vendor's file: non-empty printable ASCII, at most [`WORD_MAX`] bytes.
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

/// The plan, from `oauthAccount.organizationType` as Claude Code maps it.
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

    /// The word JSON carries.
    #[must_use]
    pub const fn word(self) -> &'static str {
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
    pub const fn title(self) -> &'static str {
        match self {
            PlanWord::Max => "Max",
            PlanWord::Pro => "Pro",
            PlanWord::Team => "Team",
            PlanWord::Enterprise => "Enterprise",
            PlanWord::Other => "other plan",
        }
    }
}

/// The plan and its tiers (design §3.3), each `None` where the profile does not say.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    /// [`PlanWord`].
    pub word: Option<PlanWord>,
    /// The served rate-limit tier.
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
    /// `profileFetchedAt`, Unix seconds: when Claude Code last fetched the profile (it refetches one 24 h old at a start).
    pub fetched_at: Option<i64>,
}

impl Plan {
    /// The plan as a person reads it.
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

/// Whether usage credits are on, from `cachedExtraUsageDisabledReason`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Credits {
    /// The key is absent, or holds neither text nor `null`.
    Unknown,
    /// `null`: credits are available.
    On,
    /// `out_of_credits`.
    Out,
    /// Any other reason ([`Word`]).
    Off(Option<Word>),
}

impl Credits {
    /// `on`, `out` or `off`; `None` for [`Credits::Unknown`].
    #[must_use]
    pub const fn state(&self) -> Option<&'static str> {
        match self {
            Credits::Unknown => None,
            Credits::On => Some("on"),
            Credits::Out => Some("out"),
            Credits::Off(_) => Some("off"),
        }
    }

    /// The vendor's reason for [`Credits::Off`].
    #[must_use]
    pub const fn reason(&self) -> Option<&Word> {
        match self {
            Credits::Off(reason) => reason.as_ref(),
            _ => None,
        }
    }
}

/// Which window a cached reading is of: aterm's closed bucket ids (design §7.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Bucket {
    /// `five_hour`.
    FiveHour,
    /// `seven_day`.
    SevenDay,
    /// `seven_day_sonnet`.
    SevenDaySonnet,
    /// `seven_day_opus`.
    SevenDayOpus,
    /// `seven_day_overage_included`: Fable's weekly bucket, every Fable name (`Fable`, `Fable 5`, …).
    Fable,
    /// `seven_day_other`: a `weekly_scoped` row of any other name.
    Other,
}

impl Bucket {
    /// The bucket's id.
    #[must_use]
    pub const fn id(self) -> &'static str {
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

/// One window as the cached `/usage` body had it.
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

/// The usage-credits spend the body carries (`extra_usage`).
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

/// Which one-time credit block.
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
    pub const fn word(self) -> &'static str {
        match self {
            OneTimeKind::CodeAndCowork => "code-and-cowork",
            OneTimeKind::Credit => "credit",
        }
    }
}

/// One one-time credit block of the body.
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

/// The `/usage` body Claude Code cached (`cachedUsageUtilization`, design F3).
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

/// How the plan bills Fable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FablePlan {
    /// Fable counts against the plan's own limits.
    Included,
    /// Fable runs on usage credits only.
    CreditsOnly,
    /// The plan's Fable limits end at a date (`None`: a date that did not read), and Fable goes to usage credits.
    LimitsEnd(Option<i64>),
    /// A flag is absent or did not read (renamed, or another shape), or the plan is not known.
    Unknown,
}

impl FablePlan {
    /// `included`, `credits-only`, `limits-end` or `unknown`.
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            FablePlan::Included => "included",
            FablePlan::CreditsOnly => "credits-only",
            FablePlan::LimitsEnd(_) => "limits-end",
            FablePlan::Unknown => "unknown",
        }
    }

    /// The date the plan's Fable limits end.
    #[must_use]
    pub const fn ends_at(&self) -> Option<i64> {
        match self {
            FablePlan::LimitsEnd(at) => *at,
            _ => None,
        }
    }
}

/// What one `.claude.json` says of its account.
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
    /// When a cached `/usage` body that names another account than this profile's was fetched
    /// (`fetchedAtMs`, Unix seconds).
    pub usage_of_another_at: Option<i64>,
    /// [`FablePlan`].
    pub fable: FablePlan,
    /// The model families a stored consent bills to usage credits with no dialog, lowercased (`fable` by `fableOverageConsentV2`, a family of `usageCreditsOnlyModelConsent`).
    pub consented: Vec<String>,
    /// The model families billed to usage credits only, lowercased (`tengu_calm_seal`); `None`: the flag absent or of another shape.
    pub credits_only: Option<Vec<String>>,
}

/// Why no profile was read, one closed word each ([`NoProfile::word`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoProfile {
    /// The home could not be resolved, so no file was looked for.
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
    pub const fn word(self) -> &'static str {
        match self {
            NoProfile::NoHome => "no-home",
            NoProfile::Missing => "no-profile",
            NoProfile::NotAFile => "not-a-file",
            NoProfile::TooLarge => "too-large",
            NoProfile::Unreadable => "unreadable",
        }
    }
}

/// The profile Claude Code keeps for the home a process's environment names.
#[must_use]
pub fn path_of(
    user_home: &Path,
    config: Option<&str>,
    home: Option<&str>,
    cwd: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    Records::at(user_home, HomeVars { config, home }, cwd).profile(user_home)
}

/// The profile of the home whose records are in `dir` ([`path_of`]).
#[must_use]
pub fn path_in(user_home: &Path, config: Option<&str>, dir: &Path) -> PathBuf {
    super::upgrade_drive::profile_in(user_home, HomeVars::config(config), dir)
}

/// Profile file Claude Code reads for the home whose plain profile is `plain` ([`path_of`]).
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

/// The key of the account signed in to the default home of a user whose home is `user_home`.
#[must_use]
pub fn default_home_key(user_home: &Path) -> Option<AccountKey> {
    let plain = path_of(user_home, None, None, || None)?;
    let records = user_home.join(".claude");
    read(&effective(&plain, Some(&records), false)).ok()?.key
}

/// The profile filename selected by a process's own exec environment.
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
    /// Read only the selector from a captured exec environment.
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

/// The process's captured profile identity for a billing decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Context {
    /// The exact plain profile path resolved from the process's HOME/config.
    pub plain: PathBuf,
    /// The process's resolved Claude record directory, for legacy selection.
    pub records: PathBuf,
    /// The filename choice from its own captured exec environment.
    pub selection: Selection,
    /// The captured HOME ([`super::accounts::HOMEDIR_ENV`]).
    pub home: Option<String>,
    /// The captured CLAUDE_CONFIG_DIR (unset/empty remain distinct).
    pub config: Option<String>,
    /// The captured stored-login selector (unset/empty remain distinct).
    pub login: Option<String>,
}

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
    /// The profile paths of already-resolved records.
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

    /// A Standard model move's complete profile environment.
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

/// Required values and removals, plus the context resolved only from those words.
pub(super) struct LaunchEnv {
    pub values: Vec<(&'static str, String)>,
    pub unset: Vec<&'static str>,
    pub context: Context,
}

/// The effective profile, with selection uncertainty preserved for a billing decision.
///
/// # Errors
///
/// An unknown selector or records directory, a legacy `.config.json` or plain profile whose
/// metadata cannot be read, or a legacy `.config.json` whose selection cannot be told.
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

/// Whether a raw `CLAUDE_CODE_CUSTOM_OAUTH_URL` renames the profile ([`effective`]).
#[must_use]
pub fn custom_oauth(raw: Option<&std::ffi::OsStr>) -> bool {
    raw.is_some_and(|url| !url.is_empty())
}

/// The method a Claude Code process's login is: a subscription for a stored login that is
/// renewable or on a known plan, else `None`.
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

/// The profile Claude Code reads for the home `env` names ([`effective`], [`path_of`]).
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

/// Whether a process's home login matches `email` ([`super::accounts::email_matches`]).
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

/// How long a cached `/usage` reading stays current ([`current_windows`]).
const USAGE_CURRENT_S: i64 = 15 * 60;

/// How long a weekly figure is shown at all ([`WeeklyFigure::words`]): past
/// [`USAGE_CURRENT_S`] with its age, past this not at all (Claude Code reuses its own
/// `/usage` cache for as long).
pub const USAGE_SHOWN_S: i64 = 60 * 60;

/// How far ahead of the reader's clock a weekly figure may be stamped and still read as just
/// read ([`WeeklyFigure::words`]): a tap reading written in the second after the resolver
/// took its clock, or a fleet peer's clock a little ahead.
const USAGE_SKEW_S: i64 = 60;

/// Facts for the requested on-screen account/subscription display.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct SubscriptionDisplay {
    /// The email the selected profile actually names; never a configured roster guess.
    pub email: Option<String>,
    /// The selected account's subscription plan.
    pub plan: Option<String>,
    /// Its weekly quota's words: a figure's under the one age rule ([`WeeklyFigure::words`]),
    /// a wall's `weekly 0% left`, or a screen's.
    pub weekly: Option<String>,
    /// The figure [`Self::weekly`] says, where a read gave one (`None`: a wall's or a screen's
    /// words, or no figure).
    pub weekly_figure: Option<WeeklyFigure>,
    /// Why there is no [`Self::weekly`] (`None`: nothing told it, and the footer says
    /// `weekly unavailable`).
    pub weekly_gap: Option<WeeklyGap>,
    /// The roster label of the account [`Self::weekly_figure`] is, where the process runs in
    /// the machine's default Claude home on a login the roster judges
    /// ([`super::accounts::judged_label`]); `None` elsewhere, where only [`Self::email`] names
    /// the account.
    pub label: Option<String>,
    /// The screen reading's placed reset, retained after expiry so an unchanged card cannot revive the same window.
    pub weekly_reset: Option<WeeklyReset>,
    /// The account the selected profile names ([`Profile::key`]): the one whose week
    /// [`Self::weekly_figure`] is. `None` where the profile names none, and for Muse Code and
    /// `agy` ([`agy_subscription_for`]), whose weeks are not a Claude plan's.
    pub account: Option<AccountKey>,
}

/// One weekly figure, whichever source read it (Claude Code's `/usage` cache, this process's usage
/// tap, the accounts store), for the one age rule ([`Self::words`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WeeklyFigure {
    /// Its share used, in tenths of a percent (0..=1000).
    pub used_x10: u32,
    /// When its window resets, Unix seconds.
    pub resets_at: i64,
    /// When it was read, Unix seconds.
    pub at: i64,
}

impl WeeklyFigure {
    /// The one age rule of every weekly source, the footer's words at `now`: `weekly N% left`
    /// while the figure is at most [`USAGE_CURRENT_S`] old (or stamped at most
    /// [`USAGE_SKEW_S`] ahead), `weekly N% left · Mm ago` up to [`USAGE_SHOWN_S`], and nothing
    /// for an older figure, one from further in the future, or one whose window has reset.
    #[must_use]
    pub fn words(&self, now: i64) -> Option<String> {
        let age = now
            .checked_sub(self.at)
            .filter(|age| (-USAGE_SKEW_S..=USAGE_SHOWN_S).contains(age))?
            .max(0);
        if self.resets_at <= now {
            return None;
        }
        let left = 1000_u32.saturating_sub(self.used_x10.min(1000));
        let left = if left.is_multiple_of(10) {
            format!("{}", left / 10)
        } else {
            format!("{}.{}", left / 10, left % 10)
        };
        Some(if age <= USAGE_CURRENT_S {
            format!("weekly {left}% left")
        } else {
            format!("weekly {left}% left \u{b7} {}m ago", age / 60)
        })
    }

    /// Whether [`Self::words`] says it at `now` with no age.
    #[must_use]
    pub fn current(&self, now: i64) -> bool {
        self.words(now).is_some() && now.saturating_sub(self.at) <= USAGE_CURRENT_S
    }

    /// The newer of `a` and `b` by [`Self::at`] (`a` on a tie).
    #[must_use]
    pub fn newer(a: Option<Self>, b: Option<Self>) -> Option<Self> {
        match (a, b) {
            (Some(a), Some(b)) => Some(if b.at > a.at { b } else { a }),
            (a, b) => a.or(b),
        }
    }
}

/// Why a Claude Code footer has no weekly figure ([`SubscriptionDisplay::weekly_gap`]). Its
/// words state the fact and never ask the person to act (owner ruling R-12: the harness reads
/// the week itself, and where it cannot, says so plainly).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WeeklyGap {
    /// `[harness] usage_tap` is off, or the harness is off for Claude Code: the harness reads
    /// no figure for it.
    TapOff,
    /// The tap is on, and no relaunch line could carry it for this process (why: the
    /// [`super::tap::Refusal`]): the harness reads no figure for it.
    Unoffered(super::tap::Refusal),
    /// The tap is on, and a relaunch line would carry it, but this process was not started
    /// with it, and aterm's restart in place is offered for it now: the next relaunch aterm
    /// makes of it (a live upgrade, a restart in place) adds it.
    Untapped,
    /// As [`Self::Untapped`], but no restart in place is offered for it now (the host
    /// supervises no such process in its tab, as with its Autopilot off, or its relaunch would
    /// not bring it back): the harness reads no figure for it.
    Unrestarted,
    /// The process carries the tap, and no reply since it started brought a weekly figure.
    NoReply,
    /// The process carries the tap, and its last weekly figure is older than
    /// [`USAGE_SHOWN_S`]: no reply in the last hour.
    Stale,
}

impl WeeklyGap {
    /// The footer's words.
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Self::TapOff => "weekly ? \u{b7} tap off",
            Self::Unoffered(_) | Self::Unrestarted => "weekly ? \u{b7} untapped",
            Self::Untapped => "weekly ? \u{b7} until relaunch",
            Self::NoReply => "weekly ? \u{b7} no reply yet",
            Self::Stale => "weekly ? \u{b7} no reply 1h",
        }
    }

    /// A screen reader's words for [`Self::words`]: the reason in full, with no glyph.
    #[must_use]
    pub const fn spoken(self) -> &'static str {
        match self {
            Self::TapOff => "weekly figure unknown, aterm's usage tap is off",
            Self::Unoffered(_) | Self::Unrestarted => {
                "weekly figure unknown, this Claude Code runs without aterm's usage tap"
            }
            Self::Untapped => "weekly figure unknown until aterm next relaunches this Claude Code",
            Self::NoReply => "weekly figure unknown, no reply has brought it yet",
            Self::Stale => "weekly figure unknown, no reply in the last hour",
        }
    }
}

/// The footer's narrow form of the weekly words [`WeeklyFigure::words`] or [`WeeklyGap::words`]
/// said, for a rule too narrow for them beside the account: an aged figure without its `weekly `
/// and ` ago` (`61.5% left \u{b7} 59m`, after the week's own mark), any reason's `weekly ?`;
/// `None` for words that are already their narrowest (a current figure, a wall's
/// `weekly 0% left`).
#[must_use]
pub fn narrow_weekly(words: &str) -> Option<String> {
    if let Some(aged) = words
        .strip_prefix("weekly ")
        .and_then(|rest| rest.strip_suffix(" ago"))
        .filter(|aged| aged.contains('%'))
    {
        return Some(aged.to_owned());
    }
    words
        .starts_with("weekly ? ")
        .then(|| "weekly ?".to_owned())
}

/// One reset occurrence, placed when the card is first observed.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct WeeklyReset {
    /// The card's reset words, which identify the observed window.
    pub words: String,
    /// Unknown words or local offset leave the occurrence unplaced.
    pub at: Option<i64>,
}

/// The plan as a dated fact (`docs/DESIGN-stats-deck-v2-2026-10-20.md` §8.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PlanUsage {
    /// The account the reading is of.
    pub account: AccountKey,
    /// The binding window: the most used of the current windows that gate the model.
    pub window: Bucket,
    /// Its share used, in tenths of a percent (0..=1000).
    pub used_x10: u16,
    /// When it resets, Unix seconds.
    pub resets_at: i64,
    /// When Claude Code fetched the reading (`fetchedAtMs`), Unix seconds.
    pub read_at: i64,
}

impl PlanUsage {
    /// Whether this reading still reflects `now` ([`plan_usage_for`], [`current_windows`]).
    #[must_use]
    pub const fn current_at(&self, now: i64) -> bool {
        self.read_at <= now
            && now.saturating_sub(self.read_at) <= USAGE_CURRENT_S
            && self.resets_at > now
    }
}

/// Read the on-screen subscription facts from this process's selected profile.
#[must_use]
pub fn subscription_for(
    env: &[String],
    user_home: &Path,
    records: &Path,
    roster: Option<&super::accounts::Roster>,
    now: i64,
) -> Option<SubscriptionDisplay> {
    selected_profile(env, user_home, records, roster).map(|selected| selected.subscription(now))
}

/// The plan usage of this process's selected profile at `now`, for the tab's `model`.
#[must_use]
pub fn plan_usage_for(
    env: &[String],
    user_home: &Path,
    records: &Path,
    roster: Option<&super::accounts::Roster>,
    now: i64,
    model: Option<&str>,
) -> Option<PlanUsage> {
    selected_profile(env, user_home, records, roster)?.plan_usage(now, model)
}

/// [`subscription_for`] and [`plan_usage_for`] from one read of the selected profile.
#[must_use]
pub fn subscription_and_plan_for(
    env: &[String],
    user_home: &Path,
    records: &Path,
    roster: Option<&super::accounts::Roster>,
    now: i64,
    model: Option<&str>,
) -> Option<(SubscriptionDisplay, Option<PlanUsage>)> {
    let selected = selected_profile(env, user_home, records, roster)?;
    let plan = selected.plan_usage(now, model);
    Some((selected.subscription(now), plan))
}

/// The account a Muse Code process is signed in as ([`SubscriptionDisplay`]).
#[must_use]
pub fn muse_subscription_for(
    argv: &[String],
    env: &[String],
    user_home: &Path,
) -> Option<SubscriptionDisplay> {
    let var = |key: &str| {
        env.iter()
            .find_map(|entry| entry.strip_prefix(key)?.strip_prefix('='))
            .filter(|value| !value.is_empty())
    };
    if var("META_API_KEY").is_some() {
        return None;
    }
    let config = match var("XDG_CONFIG_HOME").map(PathBuf::from) {
        Some(xdg) if xdg.is_absolute() => xdg,
        _ => var("HOME")
            .map_or_else(|| user_home.to_path_buf(), PathBuf::from)
            .join(".config"),
    }
    .join("muse");
    let provider = match muse_provider_arg(argv) {
        Some(provider) => Some(provider),
        None => match read_with(&config.join("settings.json"), |bytes| {
            let Obj(raw) = aterm_json::from_slice::<Obj<RawMuseSettings>>(bytes)
                .map_err(|_| NoProfile::Unreadable)?;
            match raw.ok_or(NoProfile::Unreadable)?.provider {
                Leaf::Absent => Ok(None),
                Leaf::Text(word) => Ok(word.map(|word| word.as_str().to_ascii_lowercase())),
                _ => Err(NoProfile::Unreadable),
            }
        }) {
            Ok(provider) => provider,
            Err(NoProfile::Missing) => None,
            Err(_) => return None,
        },
    };
    if provider
        .as_deref()
        .is_some_and(|provider| provider != "meta")
    {
        return None;
    }
    let email = read_with(&config.join("auth.json"), |bytes| {
        let Obj(raw) =
            aterm_json::from_slice::<Obj<RawMuseAuth>>(bytes).map_err(|_| NoProfile::Unreadable)?;
        let meta = raw.and_then(|auth| auth.providers.0).and_then(|p| p.meta.0);
        Ok(meta.and_then(|meta| {
            let oauth = meta.mechanism.0.as_deref().is_none_or(|m| m == "oauth");
            oauth
                .then_some(meta.email.0)
                .flatten()
                .filter(|email| email.contains('@') && !email.chars().any(char::is_whitespace))
        }))
    })
    .ok()??;
    Some(SubscriptionDisplay {
        email: Some(email),
        ..SubscriptionDisplay::default()
    })
}

fn muse_provider_arg(argv: &[String]) -> Option<String> {
    let config_provider = |spec: &str| {
        let (key, value) = spec.split_once('=')?;
        (key.trim() == "provider")
            .then(|| value.trim().trim_matches('"').trim().to_ascii_lowercase())
    };
    let mut provider = None;
    let mut i = 0;
    while let Some(arg) = argv.get(i) {
        if arg == "--" {
            break;
        }
        if arg == "--provider" {
            provider = argv.get(i + 1).map(|value| value.to_ascii_lowercase());
            i += 2;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--provider=") {
            provider = Some(value.to_ascii_lowercase());
        } else if matches!(arg.as_str(), "-c" | "--config") {
            if let Some(named) = argv.get(i + 1).and_then(|spec| config_provider(spec)) {
                provider = Some(named);
            }
            i += 2;
            continue;
        } else if let Some(named) = arg
            .strip_prefix("-c=")
            .or_else(|| arg.strip_prefix("--config="))
            .or_else(|| arg.strip_prefix("-c"))
            .and_then(config_provider)
        {
            provider = Some(named);
        }
        i += 1;
    }
    provider.filter(|provider| !provider.is_empty())
}

type AgyUsageCacheKey = (PathBuf, String, bool);
type AgyUsageCacheMap = std::collections::HashMap<AgyUsageCacheKey, (i64, Option<WeeklyFigure>)>;

static AGY_USAGE_CACHE: std::sync::Mutex<Option<AgyUsageCacheMap>> = std::sync::Mutex::new(None);

const AGY_USAGE_RETRY_S: i64 = 60;

/// Extracts the signed-in OAuth email from `agy`'s CLI log (`applyAuthResult: email=...`).
#[must_use]
pub fn parse_agy_auth_log(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    for line in text.lines().rev() {
        let Some((_, rest)) = line.split_once("applyAuthResult: ") else {
            continue;
        };
        let (email, after) = rest.strip_prefix("email=")?.split_once(", authMethod=")?;
        let method = after.split(',').next().unwrap_or_default().trim();
        let email = email.trim();
        return (method == "consumer"
            && email.contains('@')
            && !email.chars().any(char::is_whitespace))
        .then(|| email.to_owned());
    }
    None
}

/// Parses `agy -p /usage --output-format json` output into a [`WeeklyFigure`].
#[must_use]
pub fn parse_agy_usage_json(bytes: &[u8], third_party: bool, now: i64) -> Option<WeeklyFigure> {
    let root: aterm_json::Value = aterm_json::from_slice(bytes).ok()?;
    if root.get("status").and_then(aterm_json::Value::as_str) != Some("SUCCESS") {
        return None;
    }
    let groups = root
        .get("command")?
        .get("data")?
        .get("groups")?
        .as_array()?;
    let want_id = if third_party {
        "3p-weekly"
    } else {
        "gemini-weekly"
    };
    let bucket = groups
        .iter()
        .flat_map(|g| g.get("buckets").and_then(aterm_json::Value::as_array))
        .flatten()
        .find(|b| b.get("id").and_then(aterm_json::Value::as_str) == Some(want_id))
        .or_else(|| {
            groups
                .iter()
                .flat_map(|g| g.get("buckets").and_then(aterm_json::Value::as_array))
                .flatten()
                .find(|b| b.get("window").and_then(aterm_json::Value::as_str) == Some("weekly"))
        })?;
    let rem = bucket
        .get("remaining_fraction")
        .and_then(aterm_json::Value::as_f64)?
        .clamp(0.0, 1.0);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 0.0..=1.0"
    )]
    let left_pct = ((rem * 100.0).round() as u32).min(100);
    let used_x10 = 100u32.saturating_sub(left_pct).saturating_mul(10);
    let reset_str = bucket
        .get("reset_time")
        .and_then(aterm_json::Value::as_str)?;
    let resets_at = i64::try_from(super::upgrade_models::parse_utc(reset_str)?).ok()?;
    Some(WeeklyFigure {
        used_x10,
        resets_at,
        at: now,
    })
}

fn agy_log_scan<T, F>(argv: &[String], config: &Path, parse: F) -> Option<T>
where
    F: Fn(&[u8]) -> Option<T>,
{
    let mut log_arg = None;
    let mut i = 1;
    while let Some(arg) = argv.get(i) {
        if arg == "--" {
            break;
        }
        if matches!(arg.as_str(), "--log-file" | "-log-file") {
            log_arg = argv.get(i + 1).cloned();
            i += 2;
            continue;
        }
        if let Some(v) = arg
            .strip_prefix("--log-file=")
            .or_else(|| arg.strip_prefix("-log-file="))
        {
            log_arg = Some(v.to_owned());
        }
        i += 1;
    }
    let read_log = |path: &Path| -> Option<T> {
        const WINDOW: u64 = 128 * 1024;
        let mut f = open_nonblocking(path).ok()?;
        let meta = f.metadata().ok()?;
        if !meta.is_file() {
            return None;
        }
        let len = meta.len();
        if len > WINDOW {
            use std::io::{Seek, SeekFrom};
            let mut tail = Vec::new();
            let offset = i64::try_from(WINDOW).unwrap_or(131_072);
            if f.seek(SeekFrom::End(-offset)).is_ok()
                && (&mut f).take(WINDOW).read_to_end(&mut tail).is_ok()
                && let Some(found) = parse(&tail)
            {
                return Some(found);
            }
            let _ = f.seek(SeekFrom::Start(0));
        }
        let mut head = Vec::new();
        f.take(WINDOW).read_to_end(&mut head).ok()?;
        parse(&head)
    };
    if let Some(path) = log_arg.as_deref().map(Path::new)
        && let Some(found) = read_log(path)
    {
        return Some(found);
    }
    if let Some(found) = read_log(&config.join("cli.log")) {
        return Some(found);
    }
    let mut logs: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(config.join("log"))
        .ok()?
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let name = p.file_name()?.to_str()?;
            if !name.starts_with("cli-")
                || !Path::new(name)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("log"))
            {
                return None;
            }
            let meta = e.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            Some((meta.modified().ok()?, p))
        })
        .collect();
    logs.sort_by_key(|a| std::cmp::Reverse(a.0));
    logs.into_iter().take(3).find_map(|(_, p)| read_log(&p))
}

fn agy_log_email(argv: &[String], config: &Path) -> Option<String> {
    agy_log_scan(argv, config, parse_agy_auth_log)
}

/// Parses an `agy` or `muse` model label such as `"Gemini 4 Argon (High)"` into
/// `(model, effort)` normalized with [`super::footer::footer_model_name`] and
/// [`super::footer::effort_level`].
#[must_use]
pub fn parse_agy_model_label(label: &str) -> Option<(String, Option<String>)> {
    let t = label.trim();
    if t.is_empty() || t.contains('\u{2026}') {
        return None;
    }
    if let Some((model, effort)) = t.strip_suffix(')').and_then(|s| s.rsplit_once(" (")) {
        let lower = effort.trim().to_ascii_lowercase();
        if let Some(level) = super::footer::effort_level(&lower)
            && let Some(named) = super::footer::footer_model_name(model)
        {
            return Some((named, Some(level.to_owned())));
        }
    }
    super::footer::footer_model_name(t).map(|named| (named, None))
}

/// Extracts the latest propagated model override from `agy`'s CLI log
/// (`Propagating selected model override to backend: label="..."`).
#[must_use]
pub fn parse_agy_model_log(bytes: &[u8]) -> Option<(String, Option<String>)> {
    let text = String::from_utf8_lossy(bytes);
    for line in text.lines().rev() {
        let Some((_, rest)) =
            line.split_once("Propagating selected model override to backend: label=\"")
        else {
            continue;
        };
        let Some(label) = rest.strip_suffix('"').or_else(|| rest.split('"').next()) else {
            continue;
        };
        if let Some(parsed) = parse_agy_model_label(label) {
            return Some(parsed);
        }
    }
    None
}

fn non_claude_argv_model_effort(argv: &[String]) -> (Option<String>, Option<String>, bool, bool) {
    fn apply_config_kv(
        spec: &str,
        raw_model: &mut Option<String>,
        raw_effort: &mut Option<String>,
    ) {
        if let Some((key, value)) = spec.split_once('=') {
            let val = value.trim().trim_matches('"').trim();
            if !val.is_empty() {
                match key.trim() {
                    "model" => *raw_model = Some(val.to_owned()),
                    "effort" | "reasoning_effort" => *raw_effort = Some(val.to_owned()),
                    _ => {}
                }
            }
        }
    }
    let mut raw_model = None;
    let mut raw_effort = None;
    let mut i = 1;
    while let Some(arg) = argv.get(i) {
        if arg == "--" {
            break;
        }
        if matches!(arg.as_str(), "--model" | "-model" | "-m") {
            raw_model = argv.get(i + 1).cloned();
            i += 2;
            continue;
        }
        if let Some(v) = arg
            .strip_prefix("--model=")
            .or_else(|| arg.strip_prefix("-model="))
            .or_else(|| arg.strip_prefix("-m="))
        {
            raw_model = Some(v.to_owned());
        } else if matches!(
            arg.as_str(),
            "--effort" | "-effort" | "--reasoning-effort" | "-reasoning-effort"
        ) {
            raw_effort = argv.get(i + 1).cloned();
            i += 2;
            continue;
        } else if let Some(v) = arg
            .strip_prefix("--effort=")
            .or_else(|| arg.strip_prefix("-effort="))
            .or_else(|| arg.strip_prefix("--reasoning-effort="))
            .or_else(|| arg.strip_prefix("-reasoning-effort="))
        {
            raw_effort = Some(v.to_owned());
        } else if matches!(arg.as_str(), "-c" | "--config") {
            if let Some(spec) = argv.get(i + 1) {
                apply_config_kv(spec, &mut raw_model, &mut raw_effort);
            }
            i += 2;
            continue;
        } else if let Some(spec) = arg
            .strip_prefix("-c=")
            .or_else(|| arg.strip_prefix("--config="))
            .or_else(|| arg.strip_prefix("-c"))
        {
            apply_config_kv(spec, &mut raw_model, &mut raw_effort);
        }
        i += 1;
    }
    let parsed_model = raw_model.as_deref().and_then(parse_agy_model_label);
    let model = parsed_model.as_ref().map(|(m, _)| m.clone());
    let model_effort = parsed_model.and_then(|(_, e)| e);
    let explicit_effort = raw_effort
        .as_deref()
        .and_then(|e| super::footer::effort_level(&e.trim().to_ascii_lowercase()))
        .map(str::to_owned);
    let effort = explicit_effort.clone().or(model_effort);
    let model_open = model.is_none();
    let effort_open = explicit_effort.is_none() && (model_open || effort.is_none());
    (model, effort, model_open, effort_open)
}

fn settings_model_effort(path: &Path) -> (Option<String>, Option<String>) {
    let Some(s) = super::accounts::read_bounded(path, 64 * 1024)
        .ok()
        .flatten()
        .and_then(|t| aterm_json::from_str::<aterm_json::Value>(&t).ok())
    else {
        return (None, None);
    };
    let parsed = s
        .get("model")
        .and_then(aterm_json::Value::as_str)
        .and_then(parse_agy_model_label);
    let (model, label_effort) = parsed.map_or((None, None), |(m, e)| (Some(m), e));
    let explicit_effort = ["effort", "reasoning_effort", "reasoningEffort"]
        .into_iter()
        .find_map(|k| s.get(k).and_then(aterm_json::Value::as_str))
        .and_then(|e| super::footer::effort_level(&e.trim().to_ascii_lowercase()))
        .map(str::to_owned);
    (model, explicit_effort.or(label_effort))
}

/// The active `(model, effort, model_open, effort_open)` for an `agy` process:
/// pinned when passed on `argv`, else read from `<config>/settings.json` or
/// `<config>/cli.log` while remaining open to a live admitted screen card.
#[must_use]
pub fn agy_model_effort_for(
    argv: &[String],
    env: &[String],
    user_home: &Path,
) -> (Option<String>, Option<String>, bool, bool) {
    let (argv_model, argv_effort, model_open, effort_open) = non_claude_argv_model_effort(argv);
    if argv_model.is_some() && argv_effort.is_some() {
        return (argv_model, argv_effort, model_open, effort_open);
    }
    let var = |key: &str| {
        env.iter()
            .find_map(|entry| entry.strip_prefix(key)?.strip_prefix('='))
            .filter(|value| !value.is_empty())
    };
    let Some(config) = (match var("AGY_CLI_CONFIG_DIR").map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => Some(dir),
        _ => super::upgrade_agy::config_dir(argv, var("HOME"), user_home),
    }) else {
        return (argv_model, argv_effort, model_open, effort_open);
    };
    let (cfg_model, cfg_effort) = settings_model_effort(&config.join("settings.json"));
    let (log_model, log_effort) = if (argv_model.is_none() && cfg_model.is_none())
        || (argv_effort.is_none() && cfg_effort.is_none())
    {
        agy_log_scan(argv, &config, parse_agy_model_log).map_or((None, None), |(m, e)| (Some(m), e))
    } else {
        (None, None)
    };
    (
        argv_model.or(cfg_model).or(log_model),
        argv_effort.or(cfg_effort).or(log_effort),
        model_open,
        effort_open,
    )
}

/// The active `(model, effort, model_open, effort_open)` for a `muse` process:
/// pinned when passed on `argv`, else read from `<config>/settings.json` while
/// remaining open to a live admitted screen card.
#[must_use]
pub fn muse_model_effort_for(
    argv: &[String],
    env: &[String],
    user_home: &Path,
) -> (Option<String>, Option<String>, bool, bool) {
    let (argv_model, argv_effort, model_open, effort_open) = non_claude_argv_model_effort(argv);
    if argv_model.is_some() && argv_effort.is_some() {
        return (argv_model, argv_effort, model_open, effort_open);
    }
    let var = |key: &str| {
        env.iter()
            .find_map(|entry| entry.strip_prefix(key)?.strip_prefix('='))
            .filter(|value| !value.is_empty())
    };
    let config = match var("XDG_CONFIG_HOME").map(PathBuf::from) {
        Some(xdg) if xdg.is_absolute() => xdg,
        _ => var("HOME")
            .map_or_else(|| user_home.to_path_buf(), PathBuf::from)
            .join(".config"),
    }
    .join("muse");
    let (cfg_model, cfg_effort) = settings_model_effort(&config.join("settings.json"));
    (
        argv_model.or(cfg_model),
        argv_effort.or(cfg_effort),
        model_open,
        effort_open,
    )
}

// Start with the requested window, then widen for a complete record up to 4 MiB.
fn read_jsonl_tail(path: &Path, window: u64) -> Option<(Vec<u8>, u64)> {
    use std::io::{Seek, SeekFrom};
    const MAX_WINDOW: u64 = 4 * 1024 * 1024;
    let mut f = open_nonblocking(path).ok()?;
    let metadata = f.metadata().ok()?;
    if !metadata.is_file() {
        return None;
    }
    let len = metadata.len();
    let mut cur = window.clamp(1024, MAX_WINDOW);
    loop {
        let mut buf = Vec::new();
        if len > cur {
            f.seek(SeekFrom::Start(len - cur)).ok()?;
            (&mut f).take(cur).read_to_end(&mut buf).ok()?;
            if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                if !buf[pos + 1..].iter().all(u8::is_ascii_whitespace) || cur >= MAX_WINDOW {
                    buf.drain(..=pos);
                    return Some((buf, len));
                }
            } else if cur >= MAX_WINDOW {
                // A fragment of an oversized line cannot prove a complete record.
                return None;
            }
            cur = cur.saturating_mul(2).min(MAX_WINDOW);
        } else {
            f.seek(SeekFrom::Start(0)).ok()?;
            (&mut f).take(len).read_to_end(&mut buf).ok()?;
            return Some((buf, len));
        }
    }
}

fn agy_db_conversation(config: &Path, file: &Path) -> Option<String> {
    let rest = file.strip_prefix(config).ok()?;
    let parts: Vec<&str> = rest
        .components()
        .map(|c| c.as_os_str().to_str())
        .collect::<Option<_>>()?;
    let [dir, name] = parts.as_slice() else {
        return None;
    };
    let conversation = name.strip_suffix(".db")?;
    (*dir == super::upgrade_agy::CONVERSATIONS
        && super::upgrade_agy::is_conversation_uuid(conversation))
    .then(|| conversation.to_owned())
}

/// Prefer an observed unique presence lock, then a unique held conversation database.
/// The launch argument is only a fallback without either observation; it cannot prove a
/// conversation switched after launch. History, cwd and file existence do not identify it.
fn agy_active_conversation(
    argv: &[String],
    config: &Path,
    files: Option<&[PathBuf]>,
) -> Option<String> {
    let readers: [fn(&Path, &Path) -> Option<String>; 2] =
        [super::upgrade_agy::lock_conversation, agy_db_conversation];
    for identify in readers {
        let mut held = None;
        for cid in files
            .into_iter()
            .flatten()
            .filter_map(|path| identify(config, path))
        {
            if held.as_ref().is_some_and(|previous| *previous != cid) {
                return None;
            }
            held = Some(cid);
        }
        if held.is_some() {
            return held;
        }
    }
    super::upgrade_agy::resumed_conversation(argv)
        .filter(|cid| super::upgrade_agy::is_conversation_uuid(cid))
}

/// Formats the manual `agy --conversation=<uuid>` resume line using the same observed
/// process-held paths and explicit-launch fallback as [`agy_excerpts_for`], without
/// requiring a transcript file.
#[must_use]
pub fn agy_resume_for(
    argv: &[String],
    env: &[String],
    user_home: &Path,
    files: Option<&[PathBuf]>,
) -> Option<String> {
    let var = |key: &str| {
        env.iter()
            .find_map(|entry| entry.strip_prefix(key)?.strip_prefix('='))
            .filter(|value| !value.is_empty())
    };
    let default_config = super::upgrade_agy::config_dir(argv, var("HOME"), user_home);
    let config = match var("AGY_CLI_CONFIG_DIR").map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => dir,
        _ => default_config.clone()?,
    };
    let config = std::fs::canonicalize(&config).unwrap_or(config);
    let conversation = agy_active_conversation(argv, &config, files)?;
    let mut relaunch_env = Vec::new();
    if let Some(cfg) = var("AGY_CLI_CONFIG_DIR")
        && Path::new(cfg).is_absolute()
        && default_config.as_deref() != Some(Path::new(cfg))
    {
        relaunch_env.push(("AGY_CLI_CONFIG_DIR".to_owned(), cfg.to_owned()));
    }
    relaunch_env.extend(super::upgrade_agy::relaunch_env(var, user_home));
    super::upgrade_agy::told_line(&conversation, &relaunch_env, argv)
}

/// Reads the survey [`super::excerpt::Excerpts`] (`prompts`, `said`, `said_end`, `doing`) and
/// `transcript_len` for an active `agy` session from
/// `<config>/brain/<cid>/.system_generated/logs/transcript.jsonl` and `<config>/history.jsonl`.
/// `files` is the process's observed open-file paths: the conversation is the one it uniquely
/// holds (a presence lock, then a conversation database), else its launch's `--conversation`;
/// ambiguous holds read as unknown.
#[must_use]
pub fn agy_excerpts_for(
    argv: &[String],
    env: &[String],
    user_home: &Path,
    files: Option<&[PathBuf]>,
) -> Option<(super::excerpt::Excerpts, u64)> {
    const WINDOW: u64 = 64 * 1024;
    let var = |key: &str| {
        env.iter()
            .find_map(|entry| entry.strip_prefix(key)?.strip_prefix('='))
            .filter(|value| !value.is_empty())
    };
    let config = match var("AGY_CLI_CONFIG_DIR").map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => dir,
        _ => super::upgrade_agy::config_dir(argv, var("HOME"), user_home)?,
    };
    // The descriptor reader names canonical paths, as the relaunch witness does.
    let config = std::fs::canonicalize(config).ok()?;
    let conversation = agy_active_conversation(argv, &config, files)?;
    let history = read_jsonl_tail(&config.join("history.jsonl"), WINDOW);
    let transcript_path = config
        .join("brain")
        .join(&conversation)
        .join(".system_generated/logs/transcript.jsonl");
    let (transcript_tail, transcript_len) = read_jsonl_tail(&transcript_path, WINDOW)?;
    let excerpts = super::excerpt::agy_excerpts_from(
        &transcript_tail,
        history.as_ref().map(|(b, _)| b.as_slice()),
        &conversation,
    );
    Some((excerpts, transcript_len))
}

fn muse_active_session_lock(
    pid: u32,
    argv: &[String],
    env: &[String],
    user_home: &Path,
    files: Option<&[PathBuf]>,
) -> Option<(String, Option<PathBuf>)> {
    let var = |key: &str| {
        env.iter()
            .find_map(|entry| entry.strip_prefix(key)?.strip_prefix('='))
            .filter(|value| !value.is_empty())
    };
    let data = super::upgrade_muse::data_home(
        var(super::upgrade_muse::DATA_ENV),
        var(super::upgrade_muse::HOME_ENV),
        user_home,
    )?;
    let root = super::upgrade_muse::sessions_root(&data);
    let root = std::fs::canonicalize(&root).unwrap_or(root);
    let mut held: Option<(String, PathBuf)> = None;
    for lock in files.into_iter().flatten() {
        let Some(session) = super::upgrade_muse::session_of_lock(&root, lock) else {
            continue;
        };
        if let Some((_, previous)) = held.as_ref() {
            if previous == lock {
                continue;
            }
            return None;
        }
        // A held path and a complete PID record agree; neither a saved PID alone nor a
        // truncated prefix identifies this process. The shared reader rejects special files.
        let body = super::accounts::read_bounded(lock, 64 * 1024)
            .ok()
            .flatten()?;
        if super::upgrade_muse::lock_pid(&body) != Some(pid) {
            return None;
        }
        held = Some((session, lock.clone()));
    }
    if let Some((session, lock)) = held {
        return Some((session, Some(lock)));
    }
    // Only the explicit launch identity is available without an observed lock. Searching
    // for that UUID's log locates data; it does not infer identity from saved PID or time.
    let session = super::upgrade_muse::resumed_ref(argv)
        .filter(|s| super::upgrade_muse::is_session_uuid(s))?;
    let dirs = |p: &Path| -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = std::fs::read_dir(p)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        out.sort();
        out.reverse();
        out
    };
    let found = dirs(&root)
        .into_iter()
        .flat_map(|y| dirs(&y))
        .flat_map(|m| dirs(&m))
        .map(|d| d.join(&session).join(super::upgrade_muse::LOCK))
        .find(|lock| {
            super::upgrade_muse::session_of_lock(&root, lock).is_some()
                && (lock.is_file() || lock.with_file_name(super::upgrade_muse::LOG).is_file())
        });
    Some((session, found))
}

/// Formats the manual `muse resume <uuid>` line from the process's observed session lock,
/// or its explicit launch UUID when no lock is observed. Launch identity alone cannot
/// prove a session switched after launch.
#[must_use]
pub fn muse_resume_for(
    pid: u32,
    argv: &[String],
    env: &[String],
    user_home: &Path,
    files: Option<&[PathBuf]>,
) -> Option<String> {
    let var = |key: &str| {
        env.iter()
            .find_map(|entry| entry.strip_prefix(key)?.strip_prefix('='))
            .filter(|value| !value.is_empty())
    };
    let (session, _) = muse_active_session_lock(pid, argv, env, user_home, files)?;
    let relaunch_env = super::upgrade_muse::relaunch_env(var, user_home);
    super::upgrade_muse::told_line(&session, &relaunch_env)
}

/// Reads the survey [`super::excerpt::Excerpts`] (`prompts`, `said`, `said_end`,
/// `doing`) and `transcript_len` for an active `muse` session from
/// `<data>/muse/sessions/YYYY/MM/DD/<uuid>/session.jsonl`.
#[must_use]
pub fn muse_excerpts_for(
    pid: u32,
    argv: &[String],
    env: &[String],
    user_home: &Path,
    files: Option<&[PathBuf]>,
) -> Option<(super::excerpt::Excerpts, u64)> {
    const WINDOW: u64 = 64 * 1024;
    let (_, lock) = muse_active_session_lock(pid, argv, env, user_home, files)?;
    let log_path = lock?.with_file_name(super::upgrade_muse::LOG);
    let (tail, len) = read_jsonl_tail(&log_path, WINDOW)?;
    Some((super::excerpt::muse_excerpts_from(&tail), len))
}

fn agy_exe_for(argv: &[String], user_home: &Path) -> Option<PathBuf> {
    if let Some(first) = argv.first().map(Path::new)
        && first.is_absolute()
        && first.is_file()
    {
        return Some(first.to_path_buf());
    }
    let user_bin = user_home.join(".local/bin/agy");
    if user_bin.is_file() {
        return Some(user_bin);
    }
    if std::env::var_os("HOME").is_some_and(|h| Path::new(&h) == user_home)
        && let Some(layout) = atpkg::store::resolve_configured()
    {
        let managed = layout.agents_dir().join("agy");
        if managed.is_file() {
            return Some(managed);
        }
    }
    None
}

fn fetch_agy_usage_doc(
    exe: &Path,
    argv: &[String],
    cfg_var: Option<&str>,
    home: &Path,
) -> Option<Vec<u8>> {
    let mut cmd = aterm_uds::spawnfd::command(exe);
    cmd.args(super::upgrade_agy::config_flags(argv))
        .arg("--log-file")
        .arg("/dev/null")
        .args(super::agy_quota::USAGE_ARGS)
        .env("HOME", home)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::null());
    if let Some(cfg) = cfg_var {
        cmd.env("AGY_CLI_CONFIG_DIR", cfg);
    }
    let mut child = cmd.spawn().ok()?;
    let stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.take(64 * 1024).read_to_end(&mut buf);
        buf
    });
    let deadline = std::time::Instant::now() + super::agy_quota_store::LAUNCH_READ_BUDGET;
    let ok = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    };
    let bytes = reader.join().ok()?;
    ok.then_some(bytes)
}

fn agy_quota_store_for(
    argv: &[String],
    home_var: Option<&str>,
    cfg_var: Option<&str>,
    user_home: &Path,
) -> Option<super::agy_quota_store::Store> {
    if cfg_var.is_some() || !std::env::var_os("HOME").is_some_and(|h| Path::new(&h) == user_home) {
        return None;
    }
    let state = super::agy_quota_store::machine_state()?;
    let home_override = home_var.filter(|h| Path::new(h) != user_home);
    let flags = super::upgrade_agy::config_flags(argv);
    Some(super::agy_quota_store::Store::new(
        &state,
        &super::agy_quota_store::launch_key(home_override, &flags),
    ))
}

/// The account and weekly quota an `agy` process is signed in as ([`SubscriptionDisplay`]).
#[must_use]
pub fn agy_subscription_for(
    argv: &[String],
    env: &[String],
    user_home: &Path,
    prev: &SubscriptionDisplay,
    model: Option<&str>,
    now: i64,
) -> Option<SubscriptionDisplay> {
    let var = |key: &str| {
        env.iter()
            .find_map(|entry| entry.strip_prefix(key)?.strip_prefix('='))
            .filter(|value| !value.is_empty())
    };
    let store = agy_quota_store_for(argv, var("HOME"), var("AGY_CLI_CONFIG_DIR"), user_home);
    agy_subscription_with_store(argv, env, user_home, prev, model, now, store.as_ref())
}

fn agy_subscription_with_store(
    argv: &[String],
    env: &[String],
    user_home: &Path,
    prev: &SubscriptionDisplay,
    model: Option<&str>,
    now: i64,
    store: Option<&super::agy_quota_store::Store>,
) -> Option<SubscriptionDisplay> {
    let var = |key: &str| {
        env.iter()
            .find_map(|entry| entry.strip_prefix(key)?.strip_prefix('='))
            .filter(|value| !value.is_empty())
    };
    if var("GEMINI_API_KEY").is_some() || var("GOOGLE_API_KEY").is_some() {
        return None;
    }
    let home_dir = var("HOME").map_or_else(|| user_home.to_path_buf(), PathBuf::from);
    let config = match var("AGY_CLI_CONFIG_DIR").map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => dir,
        _ => super::upgrade_agy::config_dir(argv, var("HOME"), user_home)?,
    };
    let settings: Option<aterm_json::Value> =
        super::accounts::read_bounded(&config.join("settings.json"), 64 * 1024)
            .ok()
            .flatten()
            .and_then(|t| aterm_json::from_str(&t).ok());
    if settings
        .as_ref()
        .and_then(|s| s.get("auth_method").and_then(aterm_json::Value::as_str))
        .is_some_and(|m| m != "oauth" && m != "google" && m != "consumer")
    {
        return None;
    }
    let email = agy_log_email(argv, &config).or_else(|| prev.email.clone())?;
    let active_model = model.map(str::to_owned).or_else(|| {
        settings
            .as_ref()
            .and_then(|s| s.get("model").and_then(aterm_json::Value::as_str))
            .map(str::to_owned)
    });
    let third_party = active_model.as_deref().is_some_and(|m| {
        let lower = m.to_ascii_lowercase();
        lower.contains("claude") || lower.contains("gpt")
    });
    let same_email = prev.email.as_deref() == Some(email.as_str());
    let account_switched = prev.email.is_some() && !same_email;
    let key = (config.clone(), email.clone(), third_party);
    let other_key = (config.clone(), email.clone(), !third_party);
    let (cached, other_cached) = {
        let guard = AGY_USAGE_CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let map = guard.as_ref();
        (
            map.and_then(|m| m.get(&key).copied()),
            map.and_then(|m| m.get(&other_key).copied()),
        )
    };
    let from_other_bucket = prev.weekly_figure.is_some_and(|fig| {
        cached.and_then(|(_, f)| f) != Some(fig) && other_cached.and_then(|(_, f)| f) == Some(fig)
    });
    let prev_figure = (same_email && !from_other_bucket)
        .then_some(prev.weekly_figure)
        .flatten();
    let figure = match cached {
        Some((_, Some(fig))) if fig.current(now) => Some(fig),
        _ => {
            let store_reading = store
                .filter(|_| !account_switched)
                .and_then(super::agy_quota_store::Store::last);
            let store_fig = store_reading
                .as_ref()
                .and_then(|r| parse_agy_usage_json(r.doc.as_bytes(), third_party, r.as_of));
            let store_other = store_reading
                .as_ref()
                .and_then(|r| parse_agy_usage_json(r.doc.as_bytes(), !third_party, r.as_of));
            let fallback = prev_figure
                .or_else(|| cached.and_then(|(_, f)| f))
                .or(store_fig);
            if let Some(fig) = store_fig.filter(|f| f.current(now)) {
                let mut guard = AGY_USAGE_CACHE
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let map = guard.get_or_insert_with(std::collections::HashMap::new);
                map.insert(key, (fig.at, Some(fig)));
                if let Some(other_fig) = store_other {
                    map.insert(other_key, (other_fig.at, Some(other_fig)));
                }
                Some(fig)
            } else if matches!(cached, Some((at, None)) if now.saturating_sub(at) < AGY_USAGE_RETRY_S)
                || (!account_switched && store.is_some_and(|s| s.recent_failure(now).is_some()))
            {
                fallback
            } else {
                let lock = store.map(super::agy_quota_store::Store::try_lock);
                if matches!(lock, Some(None)) {
                    fallback
                } else if let Some(exe) = agy_exe_for(argv, user_home) {
                    let bytes =
                        fetch_agy_usage_doc(&exe, argv, var("AGY_CLI_CONFIG_DIR"), &home_dir);
                    let fetched = bytes
                        .as_deref()
                        .and_then(|b| parse_agy_usage_json(b, third_party, now));
                    let other_fetched = bytes
                        .as_deref()
                        .and_then(|b| parse_agy_usage_json(b, !third_party, now));
                    if let (Some(store), Some(bytes)) = (store, bytes)
                        && let Ok(doc) = String::from_utf8(bytes)
                        && let Ok(quota) = super::agy_quota::parse(&doc)
                    {
                        let _ = store.keep(&super::agy_quota_store::Reading {
                            as_of: now,
                            quota,
                            doc,
                        });
                    }
                    let mut guard = AGY_USAGE_CACHE
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let map = guard.get_or_insert_with(std::collections::HashMap::new);
                    map.insert(key, (now, fetched));
                    if other_fetched.is_some() {
                        map.insert(other_key, (now, other_fetched));
                    }
                    fetched.or(fallback)
                } else {
                    fallback
                }
            }
        }
    };
    let figure = figure.filter(|f| f.words(now).is_some());
    let weekly = figure.and_then(|f| f.words(now)).or_else(|| {
        (same_email && prev.weekly_figure.is_none())
            .then(|| prev.weekly.clone())
            .flatten()
    });
    Some(SubscriptionDisplay {
        email: Some(email),
        plan: same_email.then(|| prev.plan.clone()).flatten(),
        weekly,
        weekly_figure: figure,
        weekly_reset: same_email.then(|| prev.weekly_reset.clone()).flatten(),
        ..SubscriptionDisplay::default()
    })
}

/// One read of a process's selected profile ([`selected_profile`]).
struct Selected {
    /// What the profile says of its account.
    profile: Profile,
    /// The address `oauthAccount.emailAddress` names, where it reads as one.
    email: Option<String>,
}

impl Drop for Selected {
    /// An address no display took is zeroed before it is freed, as [`answers_to`]'s is.
    fn drop(&mut self) {
        if let Some(email) = self.email.take() {
            wipe(&mut email.into_bytes());
        }
    }
}

/// The one selection behind [`subscription_for`] and [`plan_usage_for`].
fn selected_profile(
    env: &[String],
    user_home: &Path,
    records: &Path,
    roster: Option<&super::accounts::Roster>,
) -> Option<Selected> {
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
    let read = read_with(&file, parse_selected).ok()?;
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
        if !read
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
    Some(read)
}

/// The profile ([`parse`]) and its address, from one file's bytes.
fn parse_selected(bytes: &[u8]) -> Result<Selected, NoProfile> {
    let profile = parse(bytes)?;
    let Obj(raw) =
        aterm_json::from_slice::<Obj<RawWho>>(bytes).map_err(|_| NoProfile::Unreadable)?;
    let email = raw
        .and_then(|who| who.oauth.0)
        .and_then(|oauth| oauth.address.0)
        .and_then(|email| {
            if super::accounts::email_refusal(&email).is_none() {
                return Some(email);
            }
            wipe(&mut email.into_bytes());
            None
        });
    Ok(Selected { profile, email })
}

impl Selected {
    fn subscription(mut self, now: i64) -> SubscriptionDisplay {
        let figure = self
            .profile
            .usage
            .as_ref()
            .and_then(|usage| cached_weekly(usage, now));
        let weekly = figure.and_then(|figure| figure.words(now));
        let plan = self
            .profile
            .plan
            .display
            .as_ref()
            .map(|name| name.as_str().to_owned())
            .or_else(|| self.profile.plan.title());
        SubscriptionDisplay {
            email: self.email.take(),
            plan,
            weekly_figure: figure.filter(|_| weekly.is_some()),
            weekly,
            weekly_gap: None,
            label: None,
            weekly_reset: None,
            account: self.profile.key,
        }
    }

    /// The window that binds `model` at `now` ([`plan_usage_for`]).
    fn plan_usage(&self, now: i64, model: Option<&str>) -> Option<PlanUsage> {
        let usage = self.profile.usage.as_ref()?;
        let own = model.and_then(model_bucket);
        let gating: Vec<&CachedWindow> = current_windows(usage, now)
            .filter(|row| {
                matches!(row.bucket, Bucket::FiveHour | Bucket::SevenDay) || Some(row.bucket) == own
            })
            .collect();
        let shown = model.map(super::footer::model_display);
        let named = |row: &CachedWindow| {
            row.bucket == Bucket::Fable
                && row
                    .name
                    .as_ref()
                    .zip(shown.as_deref())
                    .is_some_and(|(name, shown)| name.as_str().eq_ignore_ascii_case(shown))
        };
        let by_name = gating.iter().any(|row| named(row));
        let (used_x10, window) = gating
            .into_iter()
            .filter(|row| !by_name || row.bucket != Bucket::Fable || named(row))
            .map(|row| Some((row.used_pct_x10?, row)))
            .collect::<Option<Vec<_>>>()?
            .into_iter()
            .max_by_key(|(used, row)| (*used, row.resets_at))?;
        Some(PlanUsage {
            account: self.profile.key?,
            window: window.bucket,
            used_x10,
            resets_at: window.resets_at?,
            read_at: usage.fetched_at_ms.checked_div(1000)?,
        })
    }
}

fn current_windows(usage: &UsageCache, now: i64) -> impl Iterator<Item = &CachedWindow> {
    let current = usage
        .fetched_at_ms
        .checked_div(1000)
        .is_some_and(|observed| observed <= now && now.saturating_sub(observed) <= USAGE_CURRENT_S);
    let rows = if current {
        usage.windows.as_slice()
    } else {
        &[]
    };
    rows.iter()
        .filter(move |row| row.resets_at.is_some_and(|reset| reset > now))
}

/// The `seven_day` row of a cached `/usage` reading whose window still runs at `now`, as
/// read when Claude Code fetched it: its age is [`WeeklyFigure::words`]' to judge, not
/// [`current_windows`]'.
fn cached_weekly(usage: &UsageCache, now: i64) -> Option<WeeklyFigure> {
    let row = usage.windows.iter().find(|row| {
        row.bucket == Bucket::SevenDay && row.resets_at.is_some_and(|reset| reset > now)
    })?;
    Some(WeeklyFigure {
        used_x10: u32::from(row.used_pct_x10?),
        resets_at: row.resets_at?,
        at: usage.fetched_at_ms.checked_div(1000)?,
    })
}

fn model_bucket(model: &str) -> Option<Bucket> {
    let short = super::usage::model_short(model);
    let family: String = short
        .chars()
        .take_while(char::is_ascii_alphabetic)
        .collect();
    match family.to_ascii_lowercase().as_str() {
        "opus" => Some(Bucket::SevenDayOpus),
        "sonnet" => Some(Bucket::SevenDaySonnet),
        "fable" => Some(Bucket::Fable),
        _ => None,
    }
}

/// Read the profile at `path` ([`parse`]).
/// # Errors
/// [`NoProfile`]: why nothing was read.
pub fn read(path: &Path) -> Result<Profile, NoProfile> {
    read_with(path, parse)
}

/// [`read`]'s file handling, its bytes handed to `parse` before they are zeroed.
fn read_with<T>(
    path: &Path,
    parse: impl FnOnce(&[u8]) -> Result<T, NoProfile>,
) -> Result<T, NoProfile> {
    let mut file = match open_nonblocking(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(NoProfile::Missing),
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

/// Read a profile from its bytes.
/// # Errors
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
    let (usage, another) = match raw.usage.0 {
        Some(cache) if key.is_some() && cache.key.0 == key => (usage_of(cache), None),
        Some(cache) if key.is_some() && cache.key.0.is_some() => (None, Some(cache)),
        _ => (None, None),
    };
    let usage_of_another_at = another.and_then(|cache| cache.fetched_ms.num().and_then(ms_to_s));
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
        usage_of_another_at,
        fable,
        consented,
        credits_only,
    }
}

fn tier_word(raw: Word) -> Word {
    let multiple = raw
        .as_str()
        .rsplit_once('_')
        .map(|(_, tail)| tail)
        .filter(|tail| is_multiple(tail));
    multiple.and_then(Word::new).unwrap_or(raw)
}

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

fn pct_x10(pct: f64) -> Option<u16> {
    if !pct.is_finite() || pct < 0.0 {
        return None;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "0..=1000"
    )]
    Some((pct * 10.0).round().min(1000.0) as u16)
}

fn whole(n: f64) -> Option<i64> {
    #[expect(clippy::cast_possible_truncation, reason = "range checked")]
    (n.is_finite() && n.abs() < 9.0e15).then(|| n.round() as i64)
}

fn ms_to_s(ms: f64) -> Option<i64> {
    whole(ms).map(|ms| ms.div_euclid(1000))
}

/// An instant as the vendor writes one (a date, a `Z` time, or a time with a `±HH:MM` offset), in
/// Unix seconds.
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

fn drain_seq<'de, A: SeqAccess<'de>>(mut seq: A) -> Result<(), A::Error> {
    while seq.next_element::<IgnoredAny>()?.is_some() {}
    Ok(())
}

fn drain_map<'de, A: MapAccess<'de>>(mut map: A) -> Result<(), A::Error> {
    while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
    Ok(())
}

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

/// Muse Code's login store (`auth.json`), the `meta` provider's address alone kept ([`RawWho`]).
#[derive(Default, Deserialize)]
#[serde(default)]
struct RawMuseAuth {
    providers: Obj<RawMuseProviders>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawMuseProviders {
    meta: Obj<RawMuseMeta>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawMuseMeta {
    mechanism: Address,
    #[serde(rename = "user_email")]
    email: Address,
}

/// Muse Code's global `settings.json`, its `provider` alone kept: what
/// tells a Meta-account process from an `echo` or `local` one running
/// under the same login store.
#[derive(Default, Deserialize)]
#[serde(default)]
struct RawMuseSettings {
    provider: Leaf,
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

/// One scalar of the allowlist, whatever its type turned out to be: an array or an object where a
/// scalar was expected is read past, unkept.
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

    const fn num(&self) -> Option<f64> {
        match self {
            Leaf::Num(n) => Some(*n),
            _ => None,
        }
    }

    const fn flag(&self) -> Option<bool> {
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

/// A Claude account id, hashed as it is read ([`AccountKey::claude`]).
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

/// An array of words: `None` unless the value is an array of strings that are all words (Claude
/// Code reads anything else as unparseable).
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

/// `tengu_saffron_lattice.perModel`, as the one answer this reader needs: whether a Fable model is
/// billed to usage credits. `Some(true)` where any entry for an id that names Fable says
/// `enabled: true` (Claude Code's own rule where two spellings of a model disagree), `Some(false)`
/// where every such entry says `false`, `None` with no such entry or a value that is not an object.
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

/// A stored consent (`{ "<org or acct:id>": true }`): whether any entry holds `true`.
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

/// The stored consents per model family ([`family`], [`Consent`]).
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
