// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a tab's CLI really runs on (`docs/DESIGN-account-discovery-2026-11-09.md`, steps 1 to
//! 3): the login file the process reads, read afresh on every pass, since a `/login` in another
//! tab rewrites it while the process runs. The roster row chosen from the environment is a
//! belief; this is the observation, and [`super::accounts::reconcile`] says where the two
//! disagree.
//!
//! One observer per CLI. Four are on the reader aterm already had: [`claude`] (its profile),
//! [`codex`] (its `auth.json`'s token claims), [`agy`] (its CLI log's sign-in record) and
//! [`muse`] (its `auth.json`). Two read a login file nothing read before: [`grok`] (its
//! `auth.json`'s account record) and [`kimi`] (the claims of its managed login's token).
//! [`pooled`] picks a pooled CLI's.
//!
//! An observer reads identity fields only (the login email and the provider key), never a
//! credential: the profile reader zeroes every buffer it read ([`super::profile::read_with`]),
//! and the two readers here wipe what they parsed of it ([`identity_in`]).
//! A missing file is [`Observation::Unread`]; a file that names no account is
//! [`Observation::SignedOut`]; one that names an account by its provider key and no address is
//! [`Observation::KeyOnly`]; a process routed past any account of its own is
//! [`Observation::Routed`]; none of them changes the belief.

use super::accounts::{AuthRoute, Cli, ProcessLogin, Reconciled, Roster, TokenEntry};
use super::journal::{
    self, LoginState, Note, ObservedCli, ObservedLogin, ObservedSrc, SeenWhy, Tab,
};
use super::profile::{self, AccountKey};
use crate::ai::connector::Region;
use aterm_json::Value;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// One login a CLI process reads, by its identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observed {
    pub cli: ObservedCli,
    /// The login email the file names.
    pub email: String,
    /// The provider's account key, where the file names one.
    pub key: Option<AccountKey>,
    /// The provider's account system the login is in, where the CLI has more than one (Kimi
    /// Code's region): each is a realm of its own, so one address in two is two accounts.
    pub region: Option<Region>,
    /// Where it was read.
    pub src: ObservedSrc,
    /// The file it was read from: the profile, the auth file or the CLI's log, or for a token
    /// route the vault entry's name.
    pub path: PathBuf,
    /// The file's modification time, Unix seconds. Claude Code rewrites its home profile while
    /// it runs (per-project state, counters, caches), so this dates the file, never a `/login`:
    /// a login that changed under a running process is known by its address changing between
    /// two reads ([`Sightings::due`]'s [`SeenWhy::Changed`]).
    pub mtime: Option<i64>,
}

/// What an observer saw of one process's login.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Observation {
    /// A login file that names an account.
    Seen(Observed),
    /// A login file that names none.
    SignedOut { path: PathBuf },
    /// A login file that names an account by its provider key and no address (a Grok Build
    /// record or a Kimi Code token with no email). No row records such a key, so it answers to
    /// none, and with no address it is never born in the registry.
    KeyOnly { key: AccountKey, path: PathBuf },
    /// The process runs on no account of its own, by a route its own environment names
    /// ([`ObservedRoute`]).
    Routed { route: ObservedRoute },
    /// No observation: no file, an environment login, a token entry no row names.
    Unread,
}

/// The route of a process that runs on no account of its own ([`Observation::Routed`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservedRoute {
    /// A gateway or a cloud provider's own endpoint answers for it (agy behind
    /// `AGY_LLM_GATEWAY_URL` or Vertex).
    Gateway,
    /// A credential in its environment signs it in ahead of any login file (Grok Build with
    /// `GROK_AUTH` or `XAI_API_KEY`).
    Env,
}

impl ObservedRoute {
    /// The route's word, as `aterm harness accounts observed` says it.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Gateway => "gateway",
            Self::Env => "env",
        }
    }
}

impl Observation {
    /// The observed login, where one was seen.
    #[must_use]
    pub const fn seen(&self) -> Option<&Observed> {
        match self {
            Self::Seen(seen) => Some(seen),
            Self::SignedOut { .. } | Self::KeyOnly { .. } | Self::Routed { .. } | Self::Unread => {
                None
            }
        }
    }

    /// The observed login email, where one was seen.
    #[must_use]
    pub fn email(&self) -> Option<&str> {
        self.seen().map(|seen| seen.email.as_str())
    }
}

/// An age in seconds, as a sighting's surfaces say it: `12 s`, `4 min`, `3 h`, `2 d`.
#[must_use]
pub fn age_words(secs: i64) -> String {
    let secs = secs.max(0);
    match secs {
        0..60 => format!("{secs} s"),
        60..3_600 => format!("{} min", secs / 60),
        3_600..86_400 => format!("{} h", secs / 3_600),
        _ => format!("{} d", secs / 86_400),
    }
}

fn unix_secs(time: std::time::SystemTime) -> Option<i64> {
    i64::try_from(time.duration_since(UNIX_EPOCH).ok()?.as_secs()).ok()
}

/// What Claude Code's process with exec environment `env` runs on, for a user whose home is
/// `user_home`, on `route` (its [`super::accounts::ProcessLogin::route`]). An `env` that carries
/// none of the profile selectors reads the default home's profile.
///
/// - On a token route the profile may be stale (`docs/DESIGN-claude-setup-token-2026-11-05.md`
///   §5.7): the account is the one whose token entry the marker names, the roster row whose
///   entry it is ([`TokenEntry::of_row`]); a marker no row names is [`Observation::Unread`].
/// - On a stored login, the profile [`profile::login_record_of`] selects for `env`: a login
///   directory's own, else the home's.
/// - Any other route (an environment token, an API key, a provider route) is unread.
#[must_use]
pub fn claude(
    env: &[String],
    user_home: &Path,
    route: AuthRoute,
    roster: Option<&Roster>,
) -> Observation {
    match route {
        AuthRoute::Outside => Observation::Unread,
        AuthRoute::VaultToken { entry } => {
            let Some(roster) = roster else {
                return Observation::Unread;
            };
            let mut named = roster
                .rows
                .iter()
                .filter(|row| TokenEntry::of_row(row) == Some(entry));
            let (Some(row), None) = (named.next(), named.next()) else {
                return Observation::Unread;
            };
            let Some(email) = row.expected_identity_email() else {
                return Observation::Unread;
            };
            Observation::Seen(Observed {
                cli: Cli::Claude.into(),
                email: email.to_owned(),
                key: None,
                region: None,
                src: ObservedSrc::VaultToken,
                path: PathBuf::from(entry.name()),
                mtime: None,
            })
        }
        AuthRoute::Stored => {
            if let Some((record, path, mtime)) = profile::login_record_of(env, user_home) {
                return Observation::Seen(Observed {
                    cli: Cli::Claude.into(),
                    email: record.email,
                    key: record.key,
                    region: None,
                    src: ObservedSrc::Profile,
                    path,
                    mtime: unix_secs(mtime),
                });
            }
            match profile::login_profile_of(env, user_home) {
                Some(path) if path.is_file() => Observation::SignedOut { path },
                _ => Observation::Unread,
            }
        }
    }
}

/// A login as a process started on it holds it: what its environment makes the roster believe
/// ([`ProcessLogin`]) beside what its login file says ([`Observation`]). Every reader that asks
/// which account a start, a place or a tab runs on takes one of these to
/// [`super::accounts::reconcile`] ([`Login::reconciled`]); none reads the belief alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Login {
    pub who: ProcessLogin,
    pub seen: Observation,
}

impl Login {
    /// The login a Claude Code process started with `env` (`KEY=value` entries) would run on,
    /// for a user whose home is `user_home`: an empty `env` is a plain start, which reads the
    /// default home's profile ([`claude`]). With no home nothing is observed.
    #[must_use]
    pub fn claude(env: &[String], user_home: Option<&Path>, roster: &Roster) -> Self {
        let who = ProcessLogin::of_env(env.iter().filter_map(|entry| entry.split_once('=')));
        let seen = user_home.map_or(Observation::Unread, |home| {
            claude(env, home, who.route, Some(roster))
        });
        Self { who, seen }
    }

    /// The account this login runs on ([`super::accounts::reconcile`]).
    #[must_use]
    pub fn reconciled<'a>(&self, roster: &'a Roster) -> Reconciled<'a> {
        super::accounts::reconcile(roster, &self.who, &self.seen)
    }

    /// The rows signed in on this Mac, `enrolled`, as this login's place really stands
    /// (decision 6: a place is held by the account found there). Where its file names another
    /// account than the row its environment names, that row's place holds no login of the row's
    /// own, so no start lands on it; and the row the file does name is signed in here, in this
    /// place, whatever its own directory holds.
    #[must_use]
    pub fn enrolled(&self, roster: &Roster, enrolled: &BTreeSet<String>) -> BTreeSet<String> {
        let mut out = enrolled.clone();
        let reconciled = self.reconciled(roster);
        if reconciled.disagrees()
            && let Some(believed) = super::accounts::judged_label(roster, &self.who)
        {
            out.remove(believed);
        }
        if let Reconciled::Other(row) = reconciled {
            out.insert(row.label.clone());
        }
        out
    }

    /// The name a person reads this login's account by ([`super::accounts::account_name`]).
    #[must_use]
    pub fn name(&self, roster: &Roster) -> Option<String> {
        super::accounts::account_name(roster, &self.who, &self.seen)
    }
}

/// A CLI process as its observer reads it: its argv, its kernel exec environment (`KEY=value`)
/// and the directory it was started in.
#[derive(Clone, Copy, Debug)]
pub struct Process<'a> {
    pub argv: &'a [String],
    pub env: &'a [String],
    pub cwd: Option<&'a Path>,
}

fn env_var<'a>(env: &'a [String], name: &str) -> Option<&'a str> {
    env.iter().find_map(|entry| {
        entry
            .strip_prefix(name)?
            .strip_prefix('=')
            .filter(|value| !value.is_empty())
    })
}

fn mtime_of(path: &Path) -> Option<i64> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(unix_secs)
}

/// What a Codex process runs on: the ChatGPT account its `auth.json` names by its tokens'
/// claims, read with no network, no renewal and no child process
/// ([`super::codex_app_login::read_account_in_home`]; the account and user ids give the key,
/// [`super::codex_app_login::stored_login_key`]).
///
/// The file is the one the process's own exec environment selects: the login directory its
/// `CODEX_AUTH_PATH` names (`who.login`, [`ProcessLogin::of_env_for`]), else `auth.json` in its
/// `CODEX_HOME`, else in `~/.codex` ([`super::codex_rollout::codex_home`]). A process off its
/// stored login (`who.route`: an API key or another credential variable, or an auth path that
/// names no login file) is unread, as is one with no auth file; a file that names no ChatGPT
/// account (an API-key login, tokens with no email, tokens of two users) is signed out.
#[must_use]
pub fn codex(process: &Process<'_>, user_home: &Path, who: &ProcessLogin) -> Observation {
    match who.route {
        AuthRoute::Stored => {}
        AuthRoute::Outside | AuthRoute::VaultToken { .. } => return Observation::Unread,
    }
    let home = match who.login.as_deref() {
        Some(dir) => PathBuf::from(dir),
        None => {
            let user = env_var(process.env, "HOME").or_else(|| user_home.to_str());
            let Some(home) = super::codex_rollout::codex_home(
                env_var(process.env, "CODEX_HOME"),
                user,
                process.cwd,
            ) else {
                return Observation::Unread;
            };
            home
        }
    };
    let path = super::codex_app_login::auth_path_in_home(&home);
    let email = super::codex_app_login::read_account_in_home(&home).and_then(|a| a.email);
    match email {
        Some(email) => Observation::Seen(Observed {
            cli: Cli::Codex.into(),
            email,
            key: super::codex_app_login::stored_login_key(&home),
            region: None,
            src: ObservedSrc::AuthFile,
            mtime: mtime_of(&path),
            path,
        }),
        None if path.is_file() => Observation::SignedOut { path },
        None => Observation::Unread,
    }
}

/// What an `agy` process runs on: the consumer Google account its CLI log's latest sign-in
/// names ([`profile::agy_login_in_dir`], the footer's own read). Behind a gateway or Vertex it
/// runs on no account of its own ([`ObservedRoute::Gateway`]); with another provider or sign-in
/// method, unreadable settings or no complete sign-in in its log, it is unread.
#[must_use]
pub fn agy(process: &Process<'_>, user_home: &Path) -> Observation {
    match profile::agy_login_in_dir(process.argv, process.env, process.cwd, user_home) {
        profile::AgyLogin::Seen { email, path } => Observation::Seen(Observed {
            cli: Cli::Agy.into(),
            email,
            key: None,
            region: None,
            src: ObservedSrc::CliLog,
            mtime: mtime_of(&path),
            path,
        }),
        profile::AgyLogin::Gateway => Observation::Routed {
            route: ObservedRoute::Gateway,
        },
        profile::AgyLogin::Unread => Observation::Unread,
    }
}

/// What a Muse Code process runs on: Meta's OAuth account its `auth.json` names
/// ([`profile::muse_login_for`], the footer's own read). With `META_API_KEY` set, another
/// provider selected or no readable file it is unread; a file that names no OAuth account is
/// signed out.
#[must_use]
pub fn muse(process: &Process<'_>, user_home: &Path) -> Observation {
    match profile::muse_login_for(process.argv, process.env, user_home) {
        profile::MuseLogin::Seen { email, path } => Observation::Seen(Observed {
            cli: ObservedCli::Muse,
            email,
            key: None,
            region: None,
            src: ObservedSrc::AuthFile,
            mtime: mtime_of(&path),
            path,
        }),
        profile::MuseLogin::SignedOut { path } => Observation::SignedOut { path },
        profile::MuseLogin::Unread => Observation::Unread,
    }
}

/// The account a login file names, by its identity fields alone.
#[derive(Debug, PartialEq, Eq)]
struct Identity {
    /// The provider's id of the user.
    user: String,
    /// The login address, where the file carries a well-formed one.
    email: Option<String>,
}

/// What a login file says of its account ([`identity_in`]).
#[derive(Debug, PartialEq, Eq)]
enum Named {
    /// It names this account.
    One(Identity),
    /// It holds no login.
    Nobody,
    /// It holds a login whose account its own fields do not say: a token that is no set of
    /// claims, or records of two users.
    Unsaid,
}

/// A well-formed address in `text`, trimmed.
fn address(text: &str) -> Option<String> {
    let text = text.trim();
    (text.contains('@') && super::accounts::email_refusal(text).is_none()).then(|| text.to_owned())
}

/// What the JSON login file at `path` says of its account, as `pick` reads its identity fields.
/// The file holds credentials: the buffer it was read into is zeroed
/// ([`profile::read_with`]) and so is every string parsed from it, once `pick` has copied the
/// identity fields out. None where there is no readable JSON file.
fn identity_in(path: &Path, pick: impl FnOnce(&Value) -> Named) -> Option<Named> {
    profile::read_with(path, |bytes| {
        let value: Value =
            aterm_json::from_slice(bytes).map_err(|_| profile::NoProfile::Unreadable)?;
        let named = pick(&value);
        super::cli::shared_provider::wipe_json(value);
        Ok(named)
    })
    .ok()
}

/// The observation of `cli`'s login file `path` that names `named`: the account by its address
/// where the file carries one, else by its key alone.
fn named_in(cli: Cli, region: Option<Region>, named: Option<Named>, path: PathBuf) -> Observation {
    let Some(named) = named else {
        return Observation::Unread;
    };
    let identity = match named {
        Named::One(identity) => identity,
        Named::Nobody => return Observation::SignedOut { path },
        Named::Unsaid => return Observation::Unread,
    };
    let key = match (cli, region) {
        (Cli::Grok, _) => AccountKey::grok(&identity.user),
        (Cli::Kimi, Some(region)) => AccountKey::kimi(region, &identity.user),
        (Cli::Kimi, None) | (Cli::Claude | Cli::Codex | Cli::Agy, _) => None,
    };
    match (identity.email, key) {
        (Some(email), key) => Observation::Seen(Observed {
            cli: cli.into(),
            email,
            key,
            region,
            src: ObservedSrc::AuthFile,
            mtime: mtime_of(&path),
            path,
        }),
        (None, Some(key)) => Observation::KeyOnly { key, path },
        (None, None) => Observation::Unread,
    }
}

/// The account Grok Build's `auth.json` names. The file is an object of login records, one per
/// sign-in scope (xAI's own documentation reads a record's token as
/// `."https://accounts.x.ai/sign-in".key`), and a record names its account in `user_id` and
/// `email` beside its tokens (grok 1.0.77's strings; no signed-in file was at hand to measure).
/// A record at the top level is read the same way. Records of two users leave the account
/// unsaid, since nothing in the file says which one a launch runs on.
fn grok_named(file: &Value) -> Named {
    let Some(records) = file.as_object() else {
        return Named::Nobody;
    };
    let mut found: Option<Identity> = None;
    for record in std::iter::once(file).chain(records.values()) {
        let Some(user) = record["user_id"].as_str().filter(|user| !user.is_empty()) else {
            continue;
        };
        let email = record["email"].as_str().and_then(address);
        match &mut found {
            None => {
                found = Some(Identity {
                    user: user.to_owned(),
                    email,
                });
            }
            Some(first) if first.user == user => {
                if first.email.is_none() {
                    first.email = email;
                }
            }
            Some(_) => return Named::Unsaid,
        }
    }
    found.map_or(Named::Nobody, Named::One)
}

/// What a Grok Build process runs on: the xAI account its `auth.json` names ([`grok_named`]),
/// keyed by its user id ([`AccountKey::grok`]).
///
/// - A credential variable signs it in ahead of any file
///   ([`super::grok_signin::CREDENTIAL_ENV`]): it runs on no account of its own
///   ([`ObservedRoute::Env`]).
/// - The file is the one its exec environment selects, as [`super::grok_signin::signed_in`]
///   reads it: the login directory its `GROK_AUTH_PATH` names (`who.login`), else `auth.json`
///   in its `GROK_HOME`, else in `~/.grok`. An auth path that names no login file, or no file
///   there, is unread.
/// - A file grok would not start on (empty, or refused: [`super::grok_signin::login_accepted`])
///   and one with no account record are signed out.
#[must_use]
pub fn grok(process: &Process<'_>, user_home: &Path, who: &ProcessLogin) -> Observation {
    if super::grok_signin::CREDENTIAL_ENV
        .iter()
        .any(|name| env_var(process.env, name).is_some())
    {
        return Observation::Routed {
            route: ObservedRoute::Env,
        };
    }
    match who.route {
        AuthRoute::Stored => {}
        AuthRoute::Outside | AuthRoute::VaultToken { .. } => return Observation::Unread,
    }
    let home = match who.login.as_deref() {
        Some(dir) => PathBuf::from(dir),
        None => match env_var(process.env, super::grok_signin::HOME_ENV) {
            Some(dir) => PathBuf::from(dir),
            None => env_var(process.env, "HOME")
                .map_or_else(|| user_home.to_owned(), PathBuf::from)
                .join(Cli::Grok.accounts().default_home),
        },
    };
    let path = home.join("auth.json");
    if !home.is_absolute() || !path.is_file() {
        return Observation::Unread;
    }
    if !super::grok_signin::login_accepted(&path) {
        return Observation::SignedOut { path };
    }
    let named = identity_in(&path, grok_named);
    named_in(Cli::Grok, None, named, path)
}

/// The variables that move Kimi Code's managed login to other endpoints and with them the slot
/// its token is read from (`docs/measured/kimi-credentials-2026-10-24.md` §5): under one, the
/// slot `config.toml` names is not the one the process reads.
const KIMI_SLOT_ENV: [&str; 3] = [
    "KIMI_CODE_BASE_URL",
    "KIMI_CODE_OAUTH_HOST",
    "KIMI_OAUTH_HOST",
];

/// Kimi Code's `config.toml`, as far as it says which provider a turn runs on: the default
/// model, each model's provider, and the managed provider's route. Nothing else is kept, and
/// no provider's key is.
#[derive(Deserialize)]
struct RawKimiConfig {
    #[serde(default)]
    default_model: Option<String>,
    #[serde(default)]
    models: BTreeMap<String, RawKimiModel>,
    #[serde(default)]
    providers: BTreeMap<String, RawKimiProvider>,
}

#[derive(Deserialize)]
struct RawKimiModel {
    #[serde(default)]
    provider: Option<String>,
}

#[derive(Deserialize)]
struct RawKimiProvider {
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    api_key: Option<Keyed>,
    #[serde(default)]
    oauth: Option<RawKimiOauth>,
}

#[derive(Deserialize)]
struct RawKimiOauth {
    #[serde(default)]
    storage: Option<String>,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    oauth_host: Option<String>,
}

/// Whether a provider's `api_key` is a key (any text but the empty one). The key is not kept.
struct Keyed(bool);

impl<'de> Deserialize<'de> for Keyed {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Text;
        impl serde::de::Visitor<'_> for Text {
            type Value = Keyed;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a string")
            }
            fn visit_str<E: serde::de::Error>(self, text: &str) -> Result<Keyed, E> {
                Ok(Keyed(!text.is_empty()))
            }
        }
        deserializer.deserialize_str(Text)
    }
}

/// The model alias a Kimi Code command line names (`-m`, `--model`; the last one said), which
/// that invocation runs in place of the config's `default_model` (kimi 2.5.2's `--help`).
fn kimi_model_arg(argv: &[String]) -> Option<&str> {
    let mut model = None;
    let mut i = 1;
    while let Some(arg) = argv.get(i) {
        i += 1;
        match arg.as_str() {
            "--" => break,
            "-m" | "--model" => {
                model = argv.get(i).map(String::as_str).or(model);
                i += 1;
            }
            // Each takes a value, which is no flag whatever it says.
            "-p" | "--prompt" | "--output-format" | "--skills-dir" | "--agent" | "--agent-file"
            | "--add-dir" => i += 1,
            other => {
                if let Some(named) = other.strip_prefix("--model=") {
                    model = Some(named);
                } else if let Some(named) = other.strip_prefix("-m") {
                    model = Some(named);
                }
            }
        }
    }
    model
}

/// The account system and credential file of the managed login a Kimi Code process of the home
/// `home` runs its turns on, where `model` is the alias its command line names.
///
/// - The model it runs (`model`, else the config's `default_model`) is on the managed provider.
///   A home keeps a managed provider's table and its credential file from an earlier login
///   while its turns go to a provider of its own
///   (`docs/measured/kimi-credentials-2026-10-24.md` §1): that process uses no managed account.
/// - The managed provider is a region's native route, as aterm's own sign-in judges one
///   ([`super::cli::shared_provider::KimiManagedRoute`]): Kimi's provider at that region's
///   endpoint, with no key of its own, its login in a file in that region's slot.
/// - The credential file is that slot's, as the sign-in computes it
///   ([`super::cli::shared_provider::kimi_credential_path`]: `credentials/<slot>.json`).
///
/// None for any other home, and for one whose `config.toml` is unreadable.
fn kimi_slot(home: &Path, model: Option<&str>) -> Option<(Region, PathBuf)> {
    use super::cli::shared_provider::{KIMI_MANAGED, KimiManagedRoute};
    let region = profile::read_with(&home.join("config.toml"), |bytes| {
        let text = std::str::from_utf8(bytes).map_err(|_| profile::NoProfile::Unreadable)?;
        let config: RawKimiConfig =
            aterm_toml::from_str(text).map_err(|_| profile::NoProfile::Unreadable)?;
        let runs = model.or(config.default_model.as_deref());
        let on_managed = runs
            .and_then(|alias| config.models.get(alias))
            .and_then(|model| model.provider.as_deref())
            == Some(KIMI_MANAGED);
        Ok(config
            .providers
            .get(KIMI_MANAGED)
            .filter(|_| on_managed)
            .and_then(|provider| {
                let oauth = provider.oauth.as_ref();
                KimiManagedRoute {
                    kind: provider.kind.as_deref(),
                    base_url: provider.base_url.as_deref(),
                    keyed: provider.api_key.as_ref().is_some_and(|key| key.0),
                    storage: oauth.and_then(|oauth| oauth.storage.as_deref()),
                    slot: oauth.and_then(|oauth| oauth.key.as_deref()),
                    oauth_host: oauth.and_then(|oauth| oauth.oauth_host.as_deref()),
                }
                .region()
            }))
    })
    .ok()??;
    Some((
        region,
        super::cli::shared_provider::kimi_credential_path(home, region),
    ))
}

/// The account a Kimi Code credential file names. The file holds tokens and no profile
/// (`docs/measured/kimi-profile-2026-10-30.md`): Kimi's profile is a provider request, which an
/// observer never makes. What the file itself can say is in its access token where that is a
/// signed set of claims, whose `user_id` (else `sub`) names the user, as Kimi Code reads its own
/// token's subject (kimi 2.5.2's `stableJwtSubject`), and whose `email` is the address where it
/// carries one. A token that is no set of claims, or whose claims name no user, leaves the
/// account unsaid.
fn kimi_named(file: &Value) -> Named {
    let Some(token) = file["access_token"].as_str().filter(|t| !t.is_empty()) else {
        return Named::Nobody;
    };
    let mut parts = token.split('.');
    let (Some(_), Some(payload), Some(_), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Named::Unsaid;
    };
    let Ok(decoded) = aterm_codec::base64::decode_url(payload.trim_end_matches('=')) else {
        return Named::Unsaid;
    };
    let claims = aterm_json::from_slice::<Value>(&decoded);
    drop(aterm_uds::secret::Secret::from_vec(decoded));
    let Ok(claims) = claims else {
        return Named::Unsaid;
    };
    let user = ["user_id", "sub"]
        .into_iter()
        .find_map(|claim| claims[claim].as_str().filter(|user| !user.is_empty()));
    let named = user.map_or(Named::Unsaid, |user| {
        Named::One(Identity {
            user: user.to_owned(),
            email: claims["email"].as_str().and_then(address),
        })
    });
    super::cli::shared_provider::wipe_json(claims);
    named
}

/// What a Kimi Code process runs on: the account its managed login's token names
/// ([`kimi_named`]), in the account system its home is set up for ([`kimi_slot`]), keyed by
/// that region and its user id ([`AccountKey::kimi`]). No provider is asked.
///
/// - The home is the one its exec environment selects: its `KIMI_CODE_HOME` (`who.login`),
///   else `~/.kimi-code`.
/// - A process off its stored login (`who.route`: an environment model or an `Authorization`
///   header of its own) is unread, as is one whose environment moves the managed login's
///   endpoints ([`KIMI_SLOT_ENV`]), one whose model (its `-m`, else the home's default) runs on
///   another provider than the managed one, a home whose managed provider is no region's
///   native route ([`kimi_slot`]), and one with no credential file.
/// - The model is the one the process started on: one chosen inside the session is not seen.
/// - A credential file with no token is signed out; one whose token says nothing of its
///   account is unread.
#[must_use]
pub fn kimi(process: &Process<'_>, user_home: &Path, who: &ProcessLogin) -> Observation {
    match who.route {
        AuthRoute::Stored => {}
        AuthRoute::Outside | AuthRoute::VaultToken { .. } => return Observation::Unread,
    }
    if KIMI_SLOT_ENV
        .iter()
        .any(|name| env_var(process.env, name).is_some())
    {
        return Observation::Unread;
    }
    let home = match who.login.as_deref() {
        Some(dir) => PathBuf::from(dir),
        None => env_var(process.env, "HOME")
            .map_or_else(|| user_home.to_owned(), PathBuf::from)
            .join(Cli::Kimi.accounts().default_home),
    };
    if !home.is_absolute() {
        return Observation::Unread;
    }
    let Some((region, path)) = kimi_slot(&home, kimi_model_arg(process.argv)) else {
        return Observation::Unread;
    };
    let named = identity_in(&path, kimi_named);
    named_in(Cli::Kimi, Some(region), named, path)
}

/// What a pooled CLI's process runs on, by its own observer: `who` is what its environment says
/// of its login ([`ProcessLogin::of_env_for`]) and `roster` the CLI's own.
#[must_use]
pub fn pooled(
    cli: Cli,
    process: &Process<'_>,
    user_home: &Path,
    who: &ProcessLogin,
    roster: Option<&Roster>,
) -> Observation {
    match cli {
        Cli::Claude => claude(process.env, user_home, who.route, roster),
        Cli::Codex => codex(process, user_home, who),
        Cli::Agy => agy(process, user_home),
        Cli::Grok => grok(process, user_home, who),
        Cli::Kimi => kimi(process, user_home, who),
    }
}

/// How often a running tab's sighting of one account is noted again: hourly, so the fleet's
/// view of who runs what is fresh while the journal carries few notes.
pub const SEEN_REFRESH_S: i64 = 3_600;

/// What one tab's host keeps of its sightings across passes: the last account it noted and when,
/// each (observed, believed) disagreement already noted, the session it opened in the journal
/// on an account no row lists (whose end no local row would note), and the last login it handed
/// the registry (whose tab [`Sightings::end`] takes off it).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sightings {
    last: Option<(String, i64)>,
    wrong: BTreeSet<(String, String)>,
    open: Option<Note>,
    login: Option<ObservedLogin>,
}

impl Sightings {
    /// Why `email`'s sighting at `now` is noted, or `None` where it is not yet due: the first
    /// sighting, a change of account, then at most once per [`SEEN_REFRESH_S`].
    pub fn due(&mut self, email: &str, now: i64) -> Option<SeenWhy> {
        let email = email.trim().to_ascii_lowercase();
        let why = match &self.last {
            None => SeenWhy::First,
            Some((last, _)) if *last != email => SeenWhy::Changed,
            Some((_, at)) if now.saturating_sub(*at) >= SEEN_REFRESH_S => SeenWhy::Refresh,
            Some(_) => return None,
        };
        self.last = Some((email, now));
        Some(why)
    }

    /// Whether the disagreement of `observed` with the believed row `believed` is new for this
    /// tab, marking it noted.
    pub fn wrong_once(&mut self, observed: &str, believed: &str) -> bool {
        self.wrong
            .insert((observed.trim().to_ascii_lowercase(), believed.to_owned()))
    }

    /// The `session-end` of the session [`sighting`] opened on an account no row lists, at `t`,
    /// closing it.
    fn close(&mut self, t: u64) -> Option<Note> {
        let mut end = self.open.take()?;
        end.kind = "session-end";
        end.t = t;
        Some(end)
    }

    /// The tab's session is over at `now` (its agent left for good): the `session-end` of a
    /// session it opened on an account no row lists, and its last login handed back to the
    /// registry as no longer run by this tab ([`LoginState::Idle`]), so that Mac's count of tabs
    /// on it falls. Everything kept is forgotten.
    pub fn end(&mut self, now: i64) -> Sighted {
        let mut out = Sighted::default();
        let Ok(t) = u64::try_from(now) else {
            *self = Self::default();
            return out;
        };
        out.notes.extend(self.close(t));
        out.login = self.login.take().map(|login| ObservedLogin {
            state: LoginState::Idle,
            at: t,
            ..login
        });
        *self = Self::default();
        out
    }
}

/// What one pass's sighting of a tab gives ([`sighting`]): the journal notes, and the login for
/// the registry's `account-obs` rows where a note was due.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sighted {
    pub notes: Vec<Note>,
    pub login: Option<ObservedLogin>,
}

/// The key of the row a tab was believed to run on, as a `wrong-account` names it: a pooled
/// CLI's row by its claims ([`journal::account_key`], a Kimi Code row's in its own region),
/// agy's by its provider's realm, as its observed account is keyed
/// ([`journal::observed_claims`]).
fn believed_key(cli: ObservedCli, believed: &super::accounts::Row) -> Option<journal::Key> {
    match cli {
        ObservedCli::Pool(cli @ (Cli::Claude | Cli::Codex | Cli::Grok | Cli::Kimi)) => {
            journal::account_key(cli, believed)
        }
        ObservedCli::Pool(Cli::Agy) | ObservedCli::Muse => {
            let (realm, email) =
                journal::observed_claims(cli, None, believed.expected_identity_email()?)?;
            journal::realm_key(&realm, &email)
        }
    }
}

/// [`sighting`] of a tab whose CLI keeps no pool (Muse Code): no roster holds a belief about
/// it, so its account is the observed one alone, its sighting noted when due and its session's
/// start and end keyed by that account.
#[must_use]
pub fn sighting_unpooled(
    seen: &Observation,
    sightings: &mut Sightings,
    tab: Tab,
    harness: &'static str,
    now: i64,
) -> Sighted {
    let who = ProcessLogin {
        login: None,
        default_home: true,
        route: AuthRoute::Stored,
    };
    sighting(
        Some(&Roster::default()),
        &who,
        seen,
        sightings,
        tab,
        harness,
        now,
    )
}

/// One pass's sighting of the tab `tab` whose process `who` runs and whose login file says
/// `seen`, run by `harness`, at `now`: a `seen` note when one is due ([`Sightings::due`]), and
/// the login for the registry. These two need no roster: a Mac that lists no account, or whose
/// roster could not be read (`roster` `None`), is sighted like any other.
///
/// Against a roster that was read: a `wrong-account` once per disagreement with the believed
/// row, and for an account no row lists (whose session no local row records) a `session-start`
/// keyed by the observed account, closed by a `session-end` when the tab moves to another
/// account or ends ([`Sightings::end`]). An unread roster makes neither, since a row it would
/// have named may record the session itself. Keys come from the installed journal sink; with
/// none, no note is made.
#[must_use]
pub fn sighting(
    roster: Option<&Roster>,
    who: &ProcessLogin,
    seen: &Observation,
    sightings: &mut Sightings,
    tab: Tab,
    harness: &'static str,
    now: i64,
) -> Sighted {
    let mut out = Sighted::default();
    let Some(observed) = seen.seen() else {
        return out;
    };
    let Ok(t) = u64::try_from(now) else {
        return out;
    };
    let Some((realm, email)) =
        journal::observed_claims(observed.cli, observed.region, &observed.email)
    else {
        return out;
    };
    let key = journal::realm_key(&realm, &email);
    let why = sightings.due(&observed.email, now);
    if let (Some(why), Some(key)) = (why, key) {
        out.notes.push(journal::seen_note(
            observed.cli,
            key,
            tab,
            harness,
            observed.src,
            why,
            t,
        ));
    }
    let reconciled = roster.map(|roster| (roster, super::accounts::reconcile(roster, who, seen)));
    if let Some((roster, reconciled)) = &reconciled
        && reconciled.disagrees()
        && let Some(believed) =
            super::accounts::judged_label(roster, who).and_then(|label| roster.row(label))
        && let Some(key) = key
        && sightings.wrong_once(&observed.email, &believed.label)
    {
        let to = believed_key(observed.cli, believed);
        out.notes.push(journal::wrong_account_note(
            observed.cli,
            key,
            to,
            tab,
            observed.src,
            t,
        ));
    }
    // With no roster read, whether a row lists the account is not known: a session already
    // open stays open while the tab stays on its account.
    let unlisted = reconciled
        .as_ref()
        .map(|(_, reconciled)| matches!(reconciled, Reconciled::Unlisted(_)));
    if matches!(why, Some(SeenWhy::First | SeenWhy::Changed))
        && sightings
            .open
            .is_some_and(|open| unlisted == Some(false) || open.account != key)
    {
        out.notes.extend(sightings.close(t));
    }
    if unlisted == Some(true)
        && sightings.open.is_none()
        && let Some(key) = key
    {
        let mut start = journal::seen_note(
            observed.cli,
            key,
            tab,
            harness,
            observed.src,
            SeenWhy::First,
            t,
        );
        start.kind = "session-start";
        start.why = None;
        start.src = "cli-output";
        sightings.open = Some(start);
        out.notes.push(start);
    }
    if why.is_some() {
        let login = ObservedLogin {
            cli: observed.cli,
            realm,
            email,
            state: LoginState::Running,
            tab: Some(tab),
            at: t,
            src: observed.src,
        };
        sightings.login = Some(login.clone());
        out.login = Some(login);
    }
    out
}

#[path = "observe_sweep.rs"]
pub mod sweep;

#[cfg(test)]
#[path = "observe_tests.rs"]
mod tests;
