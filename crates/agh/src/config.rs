// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! agh.toml: its rows, the validation L1–L11, the release floor, and where agh's files live.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::identity::{Profile, Source};
use crate::policy::{ChainStep, Tier};
use crate::redact::Redactor;
use crate::slug::Slug;

/// Owners held at release tier whatever agh.toml says.
pub const RELEASE_FLOOR_OWNERS: [&str; 1] = [aterm_types::channel::PUBLIC_OWNER];
/// The longest agh.toml read.
pub const CONFIG_MAX: usize = 64 * 1024;
/// The longest token file read.
pub const TOKEN_MAX: usize = 256;
/// The config file's name.
pub const CONFIG_FILE: &str = "agh.toml";

/// Where agh's files are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dirs {
    /// `<aterm config dir>/agh`: agh.toml and the token files.
    pub config: PathBuf,
    /// `<state dir>/agh`: the rate book.
    pub state: PathBuf,
    /// The home directory, for `~/…` paths.
    pub home: PathBuf,
}

impl Dirs {
    /// `aterm_types::dirs::aterm_config_dir()/agh`, `state_dir()/agh` and `home_dir()`.
    #[must_use]
    pub fn from_env() -> Option<Dirs> {
        Dirs::of(
            aterm_types::dirs::aterm_config_dir(),
            aterm_types::dirs::state_dir(),
            aterm_types::dirs::home_dir(),
        )
    }

    /// `from_env`'s rule over the three roots. A relative root (a relative `XDG_CONFIG_HOME`,
    /// which the XDG spec says to ignore) would put the credential store under the current
    /// directory, which is no root at all: `None`, so agh runs with no config, anonymous.
    fn of(config: Option<PathBuf>, state: Option<PathBuf>, home: Option<PathBuf>) -> Option<Dirs> {
        let absolute = |path: Option<PathBuf>| path.filter(|path| path.is_absolute());
        Some(Dirs {
            config: absolute(config)?.join("agh"),
            state: absolute(state)?.join("agh"),
            home: absolute(home)?,
        })
    }

    /// `<root>/config/aterm/agh`, `<root>/state/agh` and `<root>/home`: tests.
    #[must_use]
    pub fn under(root: &Path) -> Dirs {
        Dirs {
            config: root.join("config").join("aterm").join("agh"),
            state: root.join("state").join("agh"),
            home: root.join("home"),
        }
    }

    /// `<config>/tokens`.
    #[inline]
    #[must_use]
    pub fn tokens(&self) -> PathBuf {
        self.config.join("tokens")
    }

    /// `<state>/rate`.
    #[inline]
    #[must_use]
    pub fn rate(&self) -> PathBuf {
        self.state.join("rate")
    }

    /// `<config>/agh.toml`.
    #[inline]
    #[must_use]
    pub fn config_file(&self) -> PathBuf {
        self.config.join(CONFIG_FILE)
    }

    /// [`Dirs::from_env`]'s rule over explicit values, for a process about to run with them:
    /// a lane unit's environment, read at `lane install`. That is `<config dir>/agh` from
    /// `aterm_types::dirs::resolve_aterm_config_dir(xdg_config_home.map(Path::as_os_str),
    /// None, Some(home.as_os_str()))`, `<state dir>/agh` from
    /// `aterm_types::dirs::resolve_state_dir(None, StatePlatform { home: Some(home.to_path_buf()),
    /// xdg_state_home: xdg_state_home.map(Path::to_path_buf), local_app_data: None })`, and
    /// `home`. (Both resolvers take `Option<&OsStr>` or `Option<PathBuf>`, never `&Path`.)
    #[must_use]
    pub fn resolve(
        home: &Path,
        xdg_config_home: Option<&Path>,
        xdg_state_home: Option<&Path>,
    ) -> Option<Dirs> {
        use aterm_types::dirs::{StatePlatform, resolve_aterm_config_dir, resolve_state_dir};
        let config = resolve_aterm_config_dir(
            xdg_config_home.map(Path::as_os_str),
            None,
            Some(home.as_os_str()),
        );
        let state = resolve_state_dir(
            None,
            StatePlatform {
                home: Some(home.to_path_buf()),
                xdg_state_home: xdg_state_home.map(Path::to_path_buf),
                local_app_data: None,
            },
        );
        Dirs::of(config, state, Some(home.to_path_buf()))
    }

    /// `<state>/journal`: the write journals.
    #[must_use]
    pub fn journal(&self) -> PathBuf {
        self.state.join("journal")
    }

    /// `<state>/grants`: the git grants.
    #[must_use]
    pub fn grants(&self) -> PathBuf {
        self.state.join("grants")
    }

    /// `<state>/needs`: the need files.
    #[must_use]
    pub fn needs(&self) -> PathBuf {
        self.state.join("needs")
    }
}

/// agh.toml as written.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct AghConfig {
    /// `[profiles.NAME]`.
    pub profiles: BTreeMap<String, ProfileRow>,
    /// `[[owners]]`.
    pub owners: Vec<OwnerRow>,
    /// `[default]`.
    pub default: Option<DefaultRow>,
    /// `[targets]`.
    pub targets: BTreeMap<String, String>,
    /// `vault = true`: the vault's adopted rows join these (design §6.10), as they route with
    /// no agh.toml. `agh auth add` writes it when it starts an agh.toml beside a vault token.
    pub vault: bool,
    /// `[[writers]]`: unattended callers' release-tier grants (L11).
    pub writers: Vec<WriterRow>,
}

/// Where a [`Validated`] came from. Only a file grants (design §10.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConfigOrigin {
    /// `load` or `load_for_host` read it from the owner's agh.toml.
    File,
    /// `validate` made it from text a caller handed in.
    Text,
}

/// One `[[writers]]` row (design §6.2): an unattended caller, and the release-tier
/// repositories the owner opens to its writes.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct WriterRow {
    /// `[a-z][a-z0-9-]{0,31}`, in one row only.
    pub caller: String,
    /// Literal `OWNER/NAME` slugs, each a release-tier owner's.
    pub repos: Vec<String>,
}

/// One `[profiles.NAME]` row.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct ProfileRow {
    /// `token_file`: an absolute or `~/` path.
    pub token_file: Option<String>,
    /// `env`: a variable's name.
    pub env: Option<String>,
    /// `tier`: `dev`, `release` or `external`.
    pub tier: String,
    /// `login`: the login the token is bound to.
    pub login: Option<String>,
    /// `covers_all_repos`.
    pub covers_all_repos: bool,
    /// `drafts_fp`: the fingerprint a person vouched for.
    pub drafts_fp: Option<String>,
    /// Accept this profile's token file when it is group- or world-readable. Honoured only by
    /// `load_for_host`, in M2 only (design §6.6); `load` reads the key and ignores it.
    pub legacy_loose: bool,
}

/// One `[[owners]]` row.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerRow {
    /// The owner's name.
    pub name: String,
    /// The owner's tier.
    pub tier: String,
    /// The read chain: profile names and `anonymous`.
    #[serde(default)]
    pub read: Vec<String>,
    /// One profile, or `"none"` (the default).
    #[serde(default = "write_none")]
    pub write: WriteField,
}

/// `write = "p"`, or the list L4 refuses.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(untagged)]
pub enum WriteField {
    /// One profile, or `none`.
    One(String),
    /// A list, which L4 refuses.
    Many(Vec<String>),
}

/// `[default]`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefaultRow {
    /// Default `["anonymous"]`.
    #[serde(default = "default_read")]
    pub read: Vec<String>,
    /// Read only so that L4 can refuse it.
    #[serde(default)]
    pub write: Option<String>,
}

/// An owner row's `write` when it names none.
fn write_none() -> WriteField {
    WriteField::One("none".into())
}

/// `[default] read` when it names none.
fn default_read() -> Vec<String> {
    vec!["anonymous".into()]
}

/// One broken rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigFault {
    /// The rule: `L1` to `L11`.
    pub rule: &'static str,
    /// What breaks it.
    pub what: String,
}

/// An owner's validated row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnerRule {
    /// The owner's name, as written.
    pub name: String,
    /// The owner's tier.
    pub tier: Tier,
    /// The read chain.
    pub read: Vec<ChainStep>,
    /// The writer, when there is one.
    pub write: Option<String>,
}

/// agh.toml after L1–L11.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Validated {
    pub(crate) profiles: BTreeMap<String, Profile>,
    /// Keyed by the lowercased owner name.
    pub(crate) owners: BTreeMap<String, OwnerRule>,
    pub(crate) default_read: Vec<ChainStep>,
    pub(crate) targets: BTreeMap<String, Slug>,
    /// `vault = true`: vault adoption's rows join these (design §6.10).
    pub(crate) vault: bool,
    /// Where it came from: only a file grants (design §10.2).
    pub(crate) origin: ConfigOrigin,
    /// The `[[writers]]` rows: callers in file order, each row's slugs in file order.
    pub(crate) writers: Vec<(String, Vec<Slug>)>,
}

impl Validated {
    /// Where it came from.
    #[must_use]
    pub fn origin(&self) -> ConfigOrigin {
        self.origin
    }

    /// The `[[writers]]` rows: callers in file order, each row's slugs in file order.
    #[must_use]
    pub fn writers(&self) -> &[(String, Vec<Slug>)] {
        &self.writers
    }

    /// The profiles, by name.
    #[inline]
    #[must_use]
    pub const fn profiles(&self) -> &BTreeMap<String, Profile> {
        &self.profiles
    }

    /// The owners' rows, keyed by the lowercased owner name.
    #[inline]
    #[must_use]
    pub const fn owners(&self) -> &BTreeMap<String, OwnerRule> {
        &self.owners
    }

    /// `[default] read`.
    #[inline]
    #[must_use]
    pub fn default_read(&self) -> &[ChainStep] {
        &self.default_read
    }

    /// `[targets]`, by name.
    #[inline]
    #[must_use]
    pub const fn targets(&self) -> &BTreeMap<String, Slug> {
        &self.targets
    }

    /// Whether the file says `vault = true`, so vault adoption's rows join these.
    #[inline]
    #[must_use]
    pub const fn joins_vault(&self) -> bool {
        self.vault
    }
}

/// The profiles vault adoption names, which an agh.toml with `vault = true` leaves to it.
pub(crate) const VAULT_PROFILES: [&str; 2] = ["dev", "release"];

/// L1: the TOML and its schema.
/// # Errors
/// The L1 fault.
pub fn parse(text: &str) -> Result<AghConfig, ConfigFault> {
    aterm_toml::from_str::<AghConfig>(text).map_err(|e| {
        let at = e.span().map_or_else(String::new, |span| {
            format!("line {}: ", line_of(text, span.start))
        });
        ConfigFault {
            rule: "L1",
            what: format!(
                "agh.toml does not parse: {at}{}",
                Redactor::shapes(&unquoted(e.message_text().trim_end()))
            ),
        }
    })
}

/// The 1-based line that byte `offset` of `text` is on.
pub(crate) fn line_of(text: &str, offset: usize) -> usize {
    let at = offset.min(text.len());
    let at = if at == text.len() && at > 0 {
        at - 1
    } else {
        at
    };
    1 + text.as_bytes()[..at]
        .iter()
        .filter(|b| **b == b'\n')
        .count()
}

/// The shortest credential agh accepts is longer than this (a token file of 19 bytes is
/// Malformed), so a run this short is never a whole token.
const TOKEN_FLOOR: usize = 20;

/// A parser message with every value it quotes taken out. A double-quoted run (serde's
/// `string "…"`) becomes `"…"`; a backticked run becomes `` `…` `` unless it is a short key
/// (dot-separated identifier segments) or short punctuation (`` `]` ``, `` `=` ``), so
/// `` unknown field `bogus` `` still names its key; and any remaining run of
/// `[A-Za-z0-9_]` of `TOKEN_FLOOR` bytes or more becomes `…`.
fn unquoted(message: &str) -> String {
    if !message.contains(['"', '`']) {
        return mask_long_words(message).into_owned();
    }
    let mut out = String::with_capacity(message.len());
    let mut rest = message;
    while let Some(at) = rest.find(['"', '`']) {
        out.push_str(&rest[..at]);
        let quote = rest[at..].chars().next().unwrap_or('"');
        let body = &rest[at + 1..];
        let (inner, after) = closing(body, quote);
        let kept = quote == '`' && short_key_or_mark(inner);
        out.push(quote);
        out.push_str(if kept { inner } else { "…" });
        out.push(quote);
        rest = after;
    }
    out.push_str(rest);
    mask_long_words(&out).into_owned()
}

/// `body` split at its closing `quote` (a `\` escapes the next character inside a double-
/// quoted run): the quoted text and what follows the quote.
fn closing(body: &str, quote: char) -> (&str, &str) {
    let mut escaped = false;
    for (i, c) in body.char_indices() {
        if escaped {
            escaped = false;
        } else if quote == '"' && c == '\\' {
            escaped = true;
        } else if c == quote {
            return (&body[..i], &body[i + 1..]);
        }
    }
    (body, "")
}

/// Whether a backticked run may stay: under `TOKEN_FLOOR` bytes, and either dot-separated
/// `[A-Za-z0-9_-]` segments (a key such as `profiles.dev`) or only punctuation.
fn short_key_or_mark(inner: &str) -> bool {
    if inner.is_empty() || inner.len() >= TOKEN_FLOOR {
        return false;
    }
    let key = inner.split('.').all(|segment| {
        !segment.is_empty()
            && segment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    });
    key || inner.chars().all(|c| c.is_ascii_punctuation() || c == '…')
}

/// `text` with each run of `[A-Za-z0-9_]` of `TOKEN_FLOOR` bytes or more replaced by `…`.
fn mask_long_words(text: &str) -> std::borrow::Cow<'_, str> {
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    if text.len() < TOKEN_FLOOR
        || !text
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .any(|run| run.len() >= TOKEN_FLOOR)
    {
        return std::borrow::Cow::Borrowed(text);
    }
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        if word(bytes[i]) {
            i += 1;
            while i < bytes.len() && word(bytes[i]) {
                i += 1;
            }
            out.push_str(if i - start >= TOKEN_FLOOR {
                "…"
            } else {
                &text[start..i]
            });
        } else {
            i += 1;
            while i < bytes.len() && !word(bytes[i]) {
                i += 1;
            }
            out.push_str(&text[start..i]);
        }
    }
    std::borrow::Cow::Owned(out)
}

/// The chain step that sends no credential.
const ANONYMOUS: &str = "anonymous";
/// An owner row's `write` that names no writer.
const NONE: &str = "none";

/// `[A-Za-z0-9][A-Za-z0-9-]{0,38}`: a GitHub owner name (L3).
#[inline]
pub(crate) fn is_owner_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=39).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
}

/// `[a-z][a-z0-9-]{0,31}`: a profile or target name (L10).
#[inline]
pub(crate) fn is_row_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=32).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// L10's fault for a profile name, or `None` when it is one.
pub(crate) fn profile_name_fault(name: &str) -> Option<String> {
    if name == ANONYMOUS || name == NONE {
        Some(format!(
            "\"{name}\" is reserved: anonymous and none name no profile"
        ))
    } else if is_row_name(name) {
        None
    } else {
        Some(format!(
            "profile name \"{}\" is not [a-z][a-z0-9-]{{0,31}}",
            as_name(name)
        ))
    }
}

/// The fault for a `[[writers]]` caller that is not `[a-z][a-z0-9-]{0,31}`, or `None` when it
/// is one. The caller is shown as a fault shows a name, so a token typed in its place is masked.
pub(crate) fn caller_fault(caller: &str) -> Option<String> {
    (!is_row_name(caller)).then(|| {
        format!(
            "writer caller \"{}\" is not [a-z][a-z0-9-]{{0,31}}",
            as_name(caller)
        )
    })
}

/// The usage error for a profile agh.toml does not name: `no profile named {name}`, with `next`.
pub(crate) fn no_profile(name: &str, next: &str) -> crate::error::Error {
    crate::error::Error::usage(format!("no profile named {name}")).with_next(next)
}

/// 12 lowercase hex characters: a fingerprint (`identity::fingerprint`).
#[inline]
pub(crate) fn is_fingerprint(text: &str) -> bool {
    aterm_codec::hex::is_lower(text, 12)
}

/// Whether `owner` is on the release floor, ASCII case-insensitively.
#[inline]
pub(crate) fn on_floor(owner: &str) -> bool {
    RELEASE_FLOOR_OWNERS
        .iter()
        .any(|floor| floor.eq_ignore_ascii_case(owner))
}

/// The one writer an owner row names: `None` for `"none"` and for a list (L4's fault).
fn writer(row: &OwnerRow) -> Option<&str> {
    match &row.write {
        WriteField::One(name) if name != NONE => Some(name),
        WriteField::One(_) | WriteField::Many(_) => None,
    }
}

/// A file's `(dev, ino)`.
type FileId = (u64, u64);

/// The faults found so far, each pushed under its rule.
struct Faults(Vec<ConfigFault>);

impl Faults {
    /// A fault, whose every agh.toml value was interpolated as a `Shown`.
    fn push(&mut self, rule: &'static str, what: String) {
        self.0.push(ConfigFault { rule, what });
    }
}

/// One agh.toml value as a fault shows it. The values come from agh.toml (owner, profile
/// and target names, chain steps, writers, tiers, logins, target values), and a token
/// pasted into one of them would otherwise be printed verbatim: on `agh policy check`'s
/// stdout and in every verb's refusal of the file.
struct Shown<'a> {
    value: &'a str,
    rule: Option<fn(&str) -> bool>,
}

impl Shown<'_> {
    fn as_cow(&self) -> std::borrow::Cow<'_, str> {
        let named = self.rule.is_some_and(|rule| rule(self.value));
        if named && !has_hex_run(self.value) {
            std::borrow::Cow::Borrowed(self.value)
        } else {
            mask_long_words(self.value)
        }
    }
}

impl std::fmt::Display for Shown<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_cow())
    }
}

impl std::fmt::Debug for Shown<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&*self.as_cow(), f)
    }
}

/// Whether `text` holds a run of `[A-Za-z0-9_]` of `TOKEN_FLOOR` bytes or more that is all
/// hex digits: what a legacy token, or a part of one, looks like.
fn has_hex_run(text: &str) -> bool {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .any(|run| run.len() >= TOKEN_FLOOR && run.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// An agh.toml value as a valid profile's row prints it (`agh whoami`, `agh auth status`).
pub(crate) fn masked(text: &str) -> String {
    if has_hex_run(text) {
        mask_long_words(text).into_owned()
    } else {
        text.to_string()
    }
}

/// `[A-Za-z_][A-Za-z0-9_]{0,63}` with no all-hex run of `TOKEN_FLOOR` bytes or more: what a
/// profile's `env` may name (L1).
fn is_env_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=64).contains(&bytes.len())
        && (bytes[0].is_ascii_alphabetic() || bytes[0] == b'_')
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'_')
        && !has_hex_run(name)
}

/// An owner name or a login, as a fault shows it.
fn as_owner(value: &str) -> Shown<'_> {
    Shown {
        value,
        rule: Some(is_owner_name),
    }
}

/// A profile, chain step, writer or target name, as a fault shows it.
fn as_name(value: &str) -> Shown<'_> {
    Shown {
        value,
        rule: Some(is_row_name),
    }
}

/// Any other value (a tier, a target's value), as a fault shows it.
fn as_value(value: &str) -> Shown<'_> {
    Shown { value, rule: None }
}

/// What `validate` reads about the rows as it judges them.
struct Rows<'a> {
    config: &'a AghConfig,
    home: &'a Path,
}

impl Rows<'_> {
    /// The tier a profile row names, when it names one.
    fn profile_tier(&self, name: &str) -> Option<Tier> {
        Tier::parse(&self.config.profiles.get(name)?.tier)
    }

    /// Whether a profile row so named exists.
    fn known(&self, name: &str) -> bool {
        self.config.profiles.contains_key(name)
    }

    /// The tier an owner row holds at decide time: release on the floor, else its own.
    fn effective_tier(&self, row: &OwnerRow) -> Option<Tier> {
        if on_floor(&row.name) {
            Some(Tier::Release)
        } else {
            Tier::parse(&row.tier)
        }
    }

    fn l1(&self, faults: &mut Faults) {
        for (name, row) in &self.config.profiles {
            let name = as_name(name);
            if Tier::parse(&row.tier).is_none() {
                faults.push(
                    "L1",
                    format!(
                        "profile {name}: tier \"{}\" is not dev, release or external",
                        as_value(&row.tier)
                    ),
                );
            }
            match (&row.token_file, &row.env) {
                (Some(_), Some(_)) | (None, None) => faults.push(
                    "L1",
                    format!("profile {name}: name exactly one of token_file and env"),
                ),
                (Some(path), None) if expand(path, self.home).is_none() => faults.push(
                    "L1",
                    format!("profile {name}: token_file must be absolute or start with ~/"),
                ),
                (None, Some(var)) if !is_env_name(var) => faults.push(
                    "L1",
                    format!("profile {name}: env must name an environment variable"),
                ),
                _ => {}
            }
            if row
                .drafts_fp
                .as_deref()
                .is_some_and(|fp| !is_fingerprint(fp))
            {
                faults.push(
                    "L1",
                    format!("profile {name}: drafts_fp is not 12 lowercase hex characters"),
                );
            }
        }
        for row in &self.config.owners {
            let owner = as_owner(&row.name);
            if Tier::parse(&row.tier).is_none() {
                faults.push(
                    "L1",
                    format!(
                        "owner {owner}: tier \"{}\" is not dev, release or external",
                        as_value(&row.tier)
                    ),
                );
            }
            for step in &row.read {
                if step != ANONYMOUS && !self.known(step) {
                    let step = as_name(step);
                    faults.push("L1", format!("owner {owner}: read names no profile {step}"));
                }
            }
            if let WriteField::One(w) = &row.write
                && w != NONE
                && !self.known(w)
            {
                let w = as_name(w);
                faults.push("L1", format!("owner {owner}: write names no profile {w}"));
            }
            let mut seen: Vec<&str> = Vec::new();
            let mut twice: Vec<&str> = Vec::new();
            for step in &row.read {
                if seen.contains(&step.as_str()) {
                    if !twice.contains(&step.as_str()) {
                        twice.push(step);
                    }
                } else {
                    seen.push(step);
                }
            }
            for step in twice {
                let step = as_name(step);
                faults.push("L1", format!("owner {owner}: read names {step} twice"));
            }
        }
        if let Some(default) = &self.config.default {
            for step in &default.read {
                if step != ANONYMOUS && !self.known(step) {
                    let step = as_name(step);
                    faults.push("L1", format!("[default]: read names no profile {step}"));
                }
            }
        }
    }

    fn l2(&self, faults: &mut Faults) {
        for row in self.config.owners.iter().filter(|row| on_floor(&row.name)) {
            let owner = as_owner(&row.name);
            if Tier::parse(&row.tier) != Some(Tier::Release) {
                faults.push(
                    "L2",
                    format!("owner {owner} is on the release floor and must be tier release"),
                );
            }
            if let Some(w) = writer(row)
                && self.profile_tier(w).is_some_and(|t| t != Tier::Release)
            {
                let w = as_name(w);
                faults.push(
                    "L2",
                    format!(
                        "owner {owner} is on the release floor; its write profile {w} must be release tier"
                    ),
                );
            }
        }
    }

    fn l3(&self, faults: &mut Faults) {
        let mut seen: Vec<String> = Vec::new();
        for row in &self.config.owners {
            let owner = as_owner(&row.name);
            if !is_owner_name(&row.name) {
                faults.push(
                    "L3",
                    format!("owner name \"{owner}\" is not a GitHub owner name"),
                );
            }
            let key = row.name.to_ascii_lowercase();
            if seen.contains(&key) {
                faults.push("L3", format!("owner {owner} has two rows"));
            } else {
                seen.push(key);
            }
        }
    }

    fn l4(&self, faults: &mut Faults) {
        for row in &self.config.owners {
            if let WriteField::Many(list) = &row.write {
                faults.push(
                    "L4",
                    format!(
                        "owner {} names {} write profiles; an owner has one writer",
                        as_owner(&row.name),
                        list.len()
                    ),
                );
            }
        }
        if self
            .config
            .default
            .as_ref()
            .is_some_and(|d| d.write.is_some())
        {
            faults.push(
                "L4",
                "[default] has no write: an owner with no row is never written to".to_string(),
            );
        }
    }

    fn l5(&self, faults: &mut Faults) {
        for (name, row) in &self.config.profiles {
            if Tier::parse(&row.tier) == Some(Tier::Release) && row.env.is_some() {
                let name = as_name(name);
                faults.push(
                    "L5",
                    format!("profile {name}: a release-tier profile reads a token file, never env"),
                );
            }
        }
    }

    fn l6(&self, faults: &mut Faults) {
        let is_release = |name: &str| self.profile_tier(name) == Some(Tier::Release);
        for row in &self.config.owners {
            let owner = as_owner(&row.name);
            let written = Tier::parse(&row.tier);
            if let Some(w) = writer(row)
                && let (Some(tier), Some(wtier)) = (written, self.profile_tier(w))
                && tier != wtier
            {
                faults.push(
                    "L6",
                    format!(
                        "owner {owner} ({}): its write profile {} is {}",
                        tier.slug(),
                        as_name(w),
                        wtier.slug()
                    ),
                );
            }
            if self.effective_tier(row) == Some(Tier::Release) {
                continue;
            }
            for step in row.read.iter().filter(|step| is_release(step)) {
                let step = as_name(step);
                faults.push(
                    "L6",
                    format!(
                        "owner {owner} is not release tier, but its read names release-tier profile {step}"
                    ),
                );
            }
            if let Some(w) = writer(row)
                && is_release(w)
            {
                let w = as_name(w);
                faults.push(
                    "L6",
                    format!(
                        "owner {owner} is not release tier, but its write names release-tier profile {w}"
                    ),
                );
            }
        }
        if let Some(default) = &self.config.default {
            for step in default.read.iter().filter(|step| is_release(step)) {
                let step = as_name(step);
                faults.push(
                    "L6",
                    format!("[default]: read names release-tier profile {step}"),
                );
            }
        }
    }

    fn l7(&self, faults: &mut Faults, file_id: &dyn Fn(&Path) -> Option<FileId>) {
        let files: Vec<(&str, PathBuf, Option<FileId>)> = self
            .config
            .profiles
            .iter()
            .filter(|(_, row)| row.env.is_none())
            .filter_map(|(name, row)| {
                let path = expand(row.token_file.as_deref()?, self.home)?;
                let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
                Some((name.as_str(), canonical, file_id(&path)))
            })
            .collect();
        for (i, (a, path_a, id_a)) in files.iter().enumerate() {
            for (b, path_b, id_b) in &files[i + 1..] {
                if path_a == path_b || (id_a.is_some() && id_a == id_b) {
                    let (a, b) = (as_name(a), as_name(b));
                    faults.push("L7", format!("profiles {a} and {b} name the same file"));
                }
            }
        }
        let bound = |tier: Tier| {
            self.config
                .profiles
                .iter()
                .filter(move |(_, row)| Tier::parse(&row.tier) == Some(tier))
                .filter_map(|(name, row)| Some((name, row.login.as_deref()?)))
        };
        for (a, login_a) in bound(Tier::Dev) {
            for (b, login_b) in bound(Tier::Release) {
                if login_a.eq_ignore_ascii_case(login_b) {
                    let (a, b, login_a) = (as_name(a), as_name(b), as_owner(login_a));
                    faults.push(
                        "L7",
                        format!(
                            "profiles {a} (dev) and {b} (release) are bound to one login {login_a}"
                        ),
                    );
                }
            }
        }
    }

    fn l8(&self, faults: &mut Faults) {
        for (name, row) in &self.config.profiles {
            if Tier::parse(&row.tier) == Some(Tier::Release) && row.login.is_none() {
                let name = as_name(name);
                faults.push("L8", format!("release-tier profile {name} has no login"));
            }
        }
        for row in &self.config.owners {
            if let Some(w) = writer(row)
                && self
                    .config
                    .profiles
                    .get(w)
                    .is_some_and(|p| p.login.is_none())
            {
                faults.push(
                    "L8",
                    format!(
                        "profile {} writes for {} but has no login",
                        as_name(w),
                        as_owner(&row.name)
                    ),
                );
            }
        }
    }

    fn l9(&self, faults: &mut Faults) {
        for (name, value) in &self.config.targets {
            if Slug::parse(value).is_err() {
                let (name, value) = (as_name(name), as_value(value));
                faults.push(
                    "L9",
                    format!("target {name} = \"{value}\" is not OWNER/NAME"),
                );
            }
        }
    }

    fn l10(&self, faults: &mut Faults) {
        for name in self.config.profiles.keys() {
            if let Some(fault) = profile_name_fault(name) {
                faults.push("L10", fault);
            }
            if self.config.vault && VAULT_PROFILES.contains(&name.as_str()) {
                faults.push(
                    "L10",
                    format!(
                        "profile {} is the vault's: with vault = true, agh.toml names no profile \
                         dev or release",
                        as_name(name)
                    ),
                );
            }
        }
        for name in self.config.targets.keys() {
            if !is_row_name(name) {
                faults.push(
                    "L10",
                    format!(
                        "target name \"{}\" is not [a-z][a-z0-9-]{{0,31}}",
                        as_name(name)
                    ),
                );
            }
        }
    }

    /// The tier `owner` holds at decide time, as `Router::tier_of` judges it over these owner
    /// rows and the release floor: release on the floor, else the row's own tier, else
    /// external for an owner with no row. `None` for a row whose tier L1 already refuses.
    fn owner_tier(&self, owner: &str) -> Option<Tier> {
        if on_floor(owner) {
            return Some(Tier::Release);
        }
        match self
            .config
            .owners
            .iter()
            .find(|row| row.name.eq_ignore_ascii_case(owner))
        {
            Some(row) => Tier::parse(&row.tier),
            None => Some(Tier::External),
        }
    }

    fn l11(&self, faults: &mut Faults) {
        let mut seen: Vec<&str> = Vec::new();
        let mut twice: Vec<&str> = Vec::new();
        for (i, row) in self.config.writers.iter().enumerate() {
            let caller = row.caller.as_str();
            if !is_row_name(caller) {
                faults.push(
                    "L11",
                    format!(
                        "writers row {}: caller {:?} is not [a-z][a-z0-9-]{{0,31}}",
                        i + 1,
                        as_name(caller)
                    ),
                );
            }
            if seen.contains(&caller) {
                if !twice.contains(&caller) {
                    twice.push(caller);
                    faults.push(
                        "L11",
                        format!("writers: caller {} has two rows", as_name(caller)),
                    );
                }
            } else {
                seen.push(caller);
            }
            let shown = as_name(caller);
            if row.repos.is_empty() {
                faults.push("L11", format!("writers row for {shown}: repos is empty"));
            }
            for entry in &row.repos {
                let Ok(slug) = Slug::parse(entry) else {
                    faults.push(
                        "L11",
                        format!(
                            "writers row for {shown}: {:?} is not a literal OWNER/NAME",
                            as_value(entry)
                        ),
                    );
                    continue;
                };
                match self.owner_tier(slug.owner()) {
                    Some(Tier::Release) | None => {}
                    Some(tier) => faults.push(
                        "L11",
                        format!(
                            "writers row for {shown}: {} is not a release-tier repository ({} is {})",
                            as_value(entry),
                            as_owner(slug.owner()),
                            tier.slug()
                        ),
                    ),
                }
            }
        }
    }

    /// The rows as a `Validated`, once no rule is broken; `None` for a row no rule let
    /// through, which a fault already names.
    fn build(&self) -> Option<Validated> {
        let mut profiles = BTreeMap::new();
        for (name, row) in &self.config.profiles {
            let source = match (&row.token_file, &row.env) {
                (Some(path), None) => Source::File(expand(path, self.home)?),
                (None, Some(var)) => Source::Env(var.clone()),
                _ => return None,
            };
            let mut profile = Profile::new(name.clone(), source, Tier::parse(&row.tier)?)
                .with_covers_all_repos(row.covers_all_repos);
            if let Some(login) = &row.login {
                profile = profile.with_login(login.clone());
            }
            if let Some(fp) = &row.drafts_fp {
                profile = profile.with_drafts_fp(fp.clone());
            }
            profiles.insert(name.clone(), profile);
        }
        let steps = |names: &[String]| -> Vec<ChainStep> {
            names
                .iter()
                .map(|name| {
                    if name == ANONYMOUS {
                        ChainStep::Anonymous
                    } else {
                        ChainStep::Profile(name.clone())
                    }
                })
                .collect()
        };
        let mut owners = BTreeMap::new();
        for row in &self.config.owners {
            let rule = OwnerRule {
                name: row.name.clone(),
                tier: Tier::parse(&row.tier)?,
                read: steps(&row.read),
                write: writer(row).map(str::to_string),
            };
            owners.insert(row.name.to_ascii_lowercase(), rule);
        }
        let default_read = self
            .config
            .default
            .as_ref()
            .map_or_else(|| vec![ChainStep::Anonymous], |d| steps(&d.read));
        let mut targets = BTreeMap::new();
        for (name, value) in &self.config.targets {
            targets.insert(name.clone(), Slug::parse(value).ok()?);
        }
        let mut writers = Vec::with_capacity(self.config.writers.len());
        for row in &self.config.writers {
            let repos = row
                .repos
                .iter()
                .map(|entry| Slug::parse(entry).ok())
                .collect::<Option<Vec<Slug>>>()?;
            writers.push((row.caller.clone(), repos));
        }
        Some(Validated {
            profiles,
            owners,
            default_read,
            targets,
            vault: self.config.vault,
            origin: ConfigOrigin::Text,
            writers,
        })
    }
}

/// L1 (references and values) to L11, every fault at once, in rule order. The `Validated` it
/// makes is of origin [`ConfigOrigin::Text`], so its `[[writers]]` rows grant nothing; only
/// [`load`] and [`load_for_host`] make one of origin `File`.
/// # Errors
/// The faults.
pub fn validate(
    config: &AghConfig,
    home: &Path,
    file_id: &dyn Fn(&Path) -> Option<(u64, u64)>,
) -> Result<Validated, Vec<ConfigFault>> {
    let rows = Rows { config, home };
    let mut faults = Faults(Vec::new());
    rows.l1(&mut faults);
    rows.l2(&mut faults);
    rows.l3(&mut faults);
    rows.l4(&mut faults);
    rows.l5(&mut faults);
    rows.l6(&mut faults);
    rows.l7(&mut faults, file_id);
    rows.l8(&mut faults);
    rows.l9(&mut faults);
    rows.l10(&mut faults);
    rows.l11(&mut faults);
    if !faults.0.is_empty() {
        return Err(faults.0);
    }
    rows.build().ok_or_else(|| {
        vec![ConfigFault {
            rule: "L1",
            what: "agh.toml holds a row no rule names".to_string(),
        }]
    })
}

/// `(dev, ino)` by `std::fs::metadata` on unix; `None` elsewhere or when it fails.
#[must_use]
pub fn file_id(path: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// An absolute path, or `~/…` expanded against `home`; `None` for anything else.
#[must_use]
pub fn expand(path: &str, home: &Path) -> Option<PathBuf> {
    if let Some(rest) = path.strip_prefix("~/") {
        return Some(home.join(rest));
    }
    let path = Path::new(path);
    path.is_absolute().then(|| path.to_path_buf())
}

/// Read and validate `<dirs.config>/agh.toml`.
/// # Errors
/// Refused, `Sub::Policy(RefusalCode::ConfigInvalid)`: for a file `read_private` refuses or
/// cannot read (why `agh.toml: {error}`); for non-UTF-8 (why `agh.toml is not UTF-8`); for
/// faults (why `agh.toml breaks {n} rule(s): {rule}: {what}; …`). The next is the remedy
/// `read_config_in` gives a refused file (such as the chmods that make it private, or the link
/// to replace), else `agh policy check`.
pub fn load(dirs: &Dirs) -> Result<Option<Validated>> {
    #[cfg(unix)]
    {
        load_private(dirs, false)
            .map(|snapshot| snapshot.map(|(validated, _)| validated))
            .map_err(|refusal| refusal.error)
    }
    #[cfg(not(unix))]
    {
        let _ = dirs;
        Ok(None)
    }
}

/// [`load`] for AghHost (design §10.3): the same file and the same rules, with each profile's
/// `legacy_loose` honoured. `Ok(None)` when agh.toml is missing, and always `Ok(None)` off
/// unix.
///
/// # Errors
///
/// As [`load`].
pub fn load_for_host(dirs: &Dirs) -> Result<Option<Validated>> {
    load_for_host_faults(dirs).map_err(|refusal| refusal.error)
}

/// agh.toml refused for AghHost (`load_for_host_faults`): the error [`load_for_host`] returns,
/// and the first fault it lists, as data, so a caller writes `{rule} {what}` whole whatever
/// `what` holds. `first` is `None` for a refusal of the file itself (its mode, its owner, its
/// bytes), which names no rule.
#[derive(Clone, Debug)]
pub struct HostConfigRefusal {
    /// The refusal.
    pub error: crate::error::Error,
    /// The first fault it lists.
    pub first: Option<ConfigFault>,
}

/// [`load_for_host`], its refusal carrying the first fault as data.
///
/// # Errors
///
/// As [`load`], with the first fault beside the error.
pub fn load_for_host_faults(
    dirs: &Dirs,
) -> std::result::Result<Option<Validated>, HostConfigRefusal> {
    #[cfg(unix)]
    {
        load_private(dirs, true).map(|snapshot| snapshot.map(|(validated, _)| validated))
    }
    #[cfg(not(unix))]
    {
        let _ = dirs;
        Ok(None)
    }
}

/// One admitted file snapshot, retaining its rows for vault composition without rereading it.
pub(crate) fn load_routing_snapshot(
    dirs: &Dirs,
    honour_loose: bool,
) -> std::result::Result<Option<(Validated, AghConfig)>, HostConfigRefusal> {
    #[cfg(unix)]
    {
        load_private(dirs, honour_loose)
    }
    #[cfg(not(unix))]
    {
        let _ = (dirs, honour_loose);
        Ok(None)
    }
}

/// Make `leaf`, a directory strictly under `dirs.state`, and every directory between them,
/// as rate.rs's `try_record` makes the rate book's: `std::fs::create_dir_all` of the directory
/// above `dirs.state`, then `aterm_uds::private_file::create_private_dir` of `dirs.state` and
/// of each level below it, down to `leaf`, in that order. A level that already exists is
/// accepted when it is private and refused as `create_private_dir` refuses it, and nothing in
/// it is touched. For each level `symlink_metadata` did not find before, its
/// `create_private_dir` is followed by `aterm_durable::durable_dir` of its parent, so a new
/// directory survives a power loss. The write journal, the git grants and the need files are
/// made through it, so the first write on a fresh state directory finds its parents.
///
/// # Errors
///
/// `ErrorKind::InvalidInput` when `leaf` is not strictly under `dirs.state`, and nothing is
/// made; otherwise the first failing call's own error.
#[cfg(unix)]
pub(crate) fn make_state_subdir(dirs: &Dirs, leaf: &Path) -> std::io::Result<()> {
    use std::path::Component;
    let below = leaf.strip_prefix(&dirs.state).ok().filter(|rest| {
        !rest.as_os_str().is_empty()
            && rest
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
    });
    let Some(below) = below else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "{} is not strictly under {}",
                leaf.display(),
                dirs.state.display()
            ),
        ));
    };
    if let Some(above) = dirs.state.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(above)?;
    }
    let mut level = dirs.state.clone();
    make_private_level(&level)?;
    for part in below.components() {
        level.push(part);
        make_private_level(&level)?;
    }
    Ok(())
}

/// One level of [`make_state_subdir`]: `create_private_dir`, then, when `symlink_metadata` did
/// not find it before, `durable_dir` of its parent.
#[cfg(unix)]
fn make_private_level(dir: &Path) -> std::io::Result<()> {
    let fresh = matches!(
        std::fs::symlink_metadata(dir),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
    );
    aterm_uds::private_file::create_private_dir(dir)?;
    if fresh && let Some(parent) = dir.parent() {
        aterm_durable::durable_dir(parent)?;
    }
    Ok(())
}

/// agh.toml's text with `repo` added to `caller`'s `writers` row (`add`), or removed from it,
/// in either form the rows take: `[[writers]]` tables or an array of inline tables
/// (`writers = [{..}]`). A row's `repos` is edited in the text itself, so every comment,
/// line break and other entry in it stands: an added slug goes on a line of its own, indented
/// as the entry before it, when the closing `]` has a line of its own, and after `, ` when not;
/// a removed one takes its own line, or its comma, with it. Adding to a missing row appends
/// the row: after the last `[[writers]]` row, at the end of the inline array, or, in a file
/// with none (an empty `writers = []` included, which it replaces and whose comment lines it
/// keeps), at the end of the text, after its last table. In a file whose every line ends in
/// CRLF, each line the edit writes does too. Removing a row's last repository removes the
/// row. Each edited text is parsed again, and its rows must be the old rows with this one edit
/// made. `agh policy grant` and `revoke` call it.
///
/// # Errors
///
/// Invalid when the text does not parse, when its `writers` is neither `[[writers]]` tables
/// nor an array of inline tables, when the caller's row has a `repos` that is not an array,
/// and when that `repos` holds an entry that is not a string (which the edit would otherwise
/// drop). Refused, with nothing edited, when an inline row with no `repos` would gain one, or
/// when the edited text does not read back as that one edit (a layout the edit cannot follow).
#[cfg(unix)]
pub(crate) fn edit_writers(text: &str, caller: &str, repo: &Slug, add: bool) -> Result<String> {
    use crate::error::{Error, Kind};
    use aterm_toml::edit::{ArrayOfTables, DocumentMut, Item, Table, TableLike, Value};
    let parse = |text: &str| {
        text.parse::<DocumentMut>().map_err(|e| {
            let at = e.span().map_or_else(String::new, |span| {
                format!(" at line {}", line_of(text, span.start))
            });
            Error::new(Kind::Invalid, format!("agh.toml does not parse{at}"))
                .with_next(POLICY_CHECK)
        })
    };
    let mut doc = parse(text)?;
    let wanted = repo.to_string();
    let before = writer_rows(&doc).ok_or_else(|| {
        Error::new(
            Kind::Invalid,
            "agh.toml's writers is not [[writers]] tables or an array of inline tables",
        )
        .with_next(POLICY_CHECK)
    })?;
    let ours = |row: &dyn TableLike| row.get("caller").and_then(Item::as_str) == Some(caller);
    let new_row = || {
        format!(
            "{{ caller = {}, repos = [{}] }}",
            Value::from(caller),
            Value::from(wanted.as_str())
        )
    };
    let edited: Option<String> = if let Some(array) = doc.get("writers").and_then(Item::as_array) {
        let rows: Vec<&dyn TableLike> = array
            .iter()
            .filter_map(|value| value.as_inline_table().map(|row| row as &dyn TableLike))
            .collect();
        match rows.iter().position(|row| ours(*row)) {
            None if !add => return Ok(text.to_string()),
            None if rows.is_empty() => {
                // `DocumentMut::remove` keeps the comment lines above `writers = []`
                // (`Table::remove` would delete them with it).
                doc.remove("writers");
                Some(append_writers_row(&doc.to_string(), caller, &wanted))
            }
            None => splice_push(text, array, &new_row()),
            Some(at) => match edit_repos(text, rows[at], caller, &wanted, add)? {
                Repos::Unchanged => return Ok(text.to_string()),
                Repos::Edited(edited) => edited,
                Repos::Emptied => splice_remove(text, array, at),
                Repos::Missing => None,
            },
        }
    } else {
        let mut rows: Vec<Table> = doc
            .get("writers")
            .and_then(Item::as_array_of_tables)
            .map(|tables| tables.iter().cloned().collect())
            .unwrap_or_default();
        let mut rebuilt = |rows: Vec<Table>| {
            let root = doc.as_table_mut();
            if rows.is_empty() {
                root.remove("writers");
            } else {
                let mut array = ArrayOfTables::new();
                for row in rows {
                    array.push(row);
                }
                root.insert("writers", Item::ArrayOfTables(array));
            }
            Some(doc.to_string())
        };
        match rows.iter().position(|row| ours(row)) {
            None if !add => return Ok(text.to_string()),
            None if rows.is_empty() => Some(append_writers_row(text, caller, &wanted)),
            None => {
                let mut row = Table::new();
                row.insert("caller", Item::Value(Value::from(caller)));
                row.insert("repos", Item::Value(new_repos(&wanted)));
                rows.push(row);
                rebuilt(rows)
            }
            Some(at) => match edit_repos(text, &rows[at], caller, &wanted, add)? {
                Repos::Unchanged => return Ok(text.to_string()),
                Repos::Edited(edited) => edited,
                Repos::Emptied => {
                    rows.remove(at);
                    rebuilt(rows)
                }
                Repos::Missing => {
                    rows[at].insert("repos", Item::Value(new_repos(&wanted)));
                    rebuilt(rows)
                }
            },
        }
    };
    let expected = with_edit(before, caller, &wanted, add);
    edited
        .map(|edited| follow_crlf(text, edited))
        .filter(|edited| {
            parse(edited)
                .ok()
                .and_then(|doc| writer_rows(&doc))
                .is_some_and(|after| after == expected)
        })
        .ok_or_else(|| {
            Error::refused(format!(
                "agh.toml's writers row for {caller} cannot be edited in place (nothing is \
                 written)"
            ))
            .with_next("edit agh.toml's writers rows by hand, then run agh policy check")
        })
}

/// One `writers` row as `edit_writers` compares it: its `caller`, and its `repos` with each
/// entry that is no string as `None` (`None` for a `repos` that is missing or no array).
#[cfg(unix)]
type WriterSummary = (Option<String>, Option<Vec<Option<String>>>);

/// A document's `writers` rows, in either form; `None` when `writers` is neither.
#[cfg(unix)]
fn writer_rows(doc: &aterm_toml::edit::DocumentMut) -> Option<Vec<WriterSummary>> {
    use aterm_toml::edit::{Item, TableLike};
    let summary = |row: &dyn TableLike| -> WriterSummary {
        let repos = row.get("repos").and_then(Item::as_array).map(|array| {
            array
                .iter()
                .map(|entry| entry.as_str().map(str::to_string))
                .collect()
        });
        (
            row.get("caller").and_then(Item::as_str).map(str::to_string),
            repos,
        )
    };
    match doc.get("writers") {
        None => Some(Vec::new()),
        Some(item) => match (item.as_array_of_tables(), item.as_array()) {
            (Some(tables), _) => Some(tables.iter().map(|row| summary(row)).collect()),
            (None, Some(array)) => array
                .iter()
                .map(|value| value.as_inline_table().map(|row| summary(row)))
                .collect(),
            (None, None) => None,
        },
    }
}

/// `rows` with `wanted` added to `caller`'s row (a new row when it has none), or removed from
/// it (the row with its last entry): what `edit_writers`'s text must read back as.
#[cfg(unix)]
fn with_edit(
    mut rows: Vec<WriterSummary>,
    caller: &str,
    wanted: &str,
    add: bool,
) -> Vec<WriterSummary> {
    let held = |entry: &Option<String>| {
        entry
            .as_deref()
            .is_some_and(|entry| entry.eq_ignore_ascii_case(wanted))
    };
    let at = rows
        .iter()
        .position(|(name, _)| name.as_deref() == Some(caller));
    match (at, add) {
        (None, true) => rows.push((
            Some(caller.to_string()),
            Some(vec![Some(wanted.to_string())]),
        )),
        (None, false) => {}
        (Some(at), true) => {
            let repos = rows[at].1.get_or_insert_with(Vec::new);
            if !repos.iter().any(held) {
                repos.push(Some(wanted.to_string()));
            }
        }
        (Some(at), false) => {
            if let Some(repos) = rows[at].1.as_mut() {
                repos.retain(|entry| !held(entry));
                if repos.is_empty() {
                    rows.remove(at);
                }
            }
        }
    }
    rows
}

/// A new row's `repos`: `[wanted]`.
#[cfg(unix)]
fn new_repos(wanted: &str) -> aterm_toml::edit::Value {
    use aterm_toml::edit::{Array, Value};
    Value::from(std::iter::once(wanted).collect::<Array>())
}

/// `text` with a first `[[writers]]` row for `caller` appended at its end, after a blank line.
#[cfg(unix)]
fn append_writers_row(text: &str, caller: &str, wanted: &str) -> String {
    use aterm_toml::edit::Value;
    use std::fmt::Write as _;
    let mut appended = text.to_string();
    if !appended.is_empty() && !appended.ends_with('\n') {
        appended.push('\n');
    }
    if !appended.is_empty() {
        appended.push('\n');
    }
    let _ = write!(
        appended,
        "[[writers]]\ncaller = {}\nrepos = {}\n",
        Value::from(caller),
        new_repos(wanted)
    );
    appended
}

/// `edited` with a CR before each bare LF when every line of `original` ends in CRLF, so the
/// lines an edit wrote follow the file's; `edited` as it is otherwise (a file of LF lines, of
/// mixed lines, or of one line). Every bare LF of such an `edited` is one the edit wrote, since
/// `original` holds none; the rows it writes hold no multi-line string.
#[cfg(unix)]
fn follow_crlf(original: &str, edited: String) -> String {
    let bytes = original.as_bytes();
    let mut ends = original.match_indices('\n').map(|(at, _)| at).peekable();
    let crlf = ends.peek().is_some()
        && ends.all(|at| at.checked_sub(1).and_then(|cr| bytes.get(cr)) == Some(&b'\r'));
    if !crlf {
        return edited;
    }
    let mut out = String::with_capacity(edited.len() + 16);
    let mut previous = '\0';
    for c in edited.chars() {
        if c == '\n' && previous != '\r' {
            out.push('\r');
        }
        out.push(c);
        previous = c;
    }
    out
}

/// What `edit_repos` made of a row's `repos`.
#[cfg(unix)]
enum Repos {
    /// The edit changes nothing.
    Unchanged,
    /// The text with the edit made in place (`None` when the layout would not take it).
    Edited(Option<String>),
    /// The edit removes the row's last entry, so the row goes.
    Emptied,
    /// The row has no `repos` for the added slug to join.
    Missing,
}

/// The edit of one row's `repos` in `text`, which the row was parsed from.
#[cfg(unix)]
fn edit_repos(
    text: &str,
    row: &dyn aterm_toml::edit::TableLike,
    caller: &str,
    wanted: &str,
    add: bool,
) -> Result<Repos> {
    use crate::error::{Error, Kind};
    use aterm_toml::edit::Value;
    let invalid = |what: &str| {
        Error::new(
            Kind::Invalid,
            format!("agh.toml's writers row for {caller}: {what}"),
        )
        .with_next(POLICY_CHECK)
    };
    let Some(item) = row.get("repos") else {
        return Ok(if add {
            Repos::Missing
        } else {
            Repos::Unchanged
        });
    };
    let array = item
        .as_array()
        .ok_or_else(|| invalid("repos is not a list"))?;
    let entries: Vec<&str> = array
        .iter()
        .map(Value::as_str)
        .collect::<Option<_>>()
        .ok_or_else(|| invalid("repos holds an entry that is not a string"))?;
    let held: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.eq_ignore_ascii_case(wanted))
        .map(|(at, _)| at)
        .collect();
    Ok(match (add, held.is_empty()) {
        (true, false) | (false, true) => Repos::Unchanged,
        (true, true) => Repos::Edited(splice_push(text, array, &Value::from(wanted).to_string())),
        (false, false) if held.len() == entries.len() => Repos::Emptied,
        (false, false) => {
            Repos::Edited(held.iter().rev().try_fold(text.to_string(), |text, at| {
                splice_remove(&text, array, *at)
            }))
        }
    })
}

/// The whitespace before the value that starts at `start`, when nothing else precedes it on
/// its line; two spaces otherwise.
#[cfg(unix)]
fn indent_before(text: &str, start: usize) -> &str {
    let line = text[..start].rfind('\n').map_or(0, |at| at + 1);
    let lead = &text[line..start];
    if lead.bytes().all(|b| b == b' ' || b == b'\t') {
        lead
    } else {
        "  "
    }
}

/// Whether the trivia between an array's last value and its `]` holds the trailing comma.
#[cfg(unix)]
fn trivia_has_comma(trivia: &str) -> bool {
    let mut in_comment = false;
    for c in trivia.chars() {
        match (in_comment, c) {
            (true, '\n') => in_comment = false,
            (true, _) => {}
            (false, '#') => in_comment = true,
            (false, ',') => return true,
            (false, _) => {}
        }
    }
    false
}

/// `text` with `entry` (one value's TOML) as the last element of `array`, which was parsed
/// from `text` (see `edit_writers` for the layout). `None` when `array` carries no source
/// spans.
#[cfg(unix)]
fn splice_push(text: &str, array: &aterm_toml::edit::Array, entry: &str) -> Option<String> {
    let span = array.span()?;
    let close = span.end.checked_sub(1)?;
    let Some(last) = array.iter().last() else {
        let mut out = text.to_string();
        out.insert_str(span.start + 1, entry);
        return Some(out);
    };
    let last_span = last.span()?;
    let trivia = text.get(last_span.end..close)?;
    let comma = trivia_has_comma(trivia);
    let mut out = String::with_capacity(text.len() + entry.len() + 8);
    match trivia.rfind('\n') {
        Some(at) => {
            let line = last_span.end + at + 1;
            out.push_str(&text[..last_span.end]);
            if !comma {
                out.push(',');
            }
            out.push_str(&text[last_span.end..line]);
            out.push_str(indent_before(text, last_span.start));
            out.push_str(entry);
            if comma {
                out.push(',');
            }
            out.push('\n');
            out.push_str(&text[line..]);
        }
        None => {
            out.push_str(&text[..last_span.end]);
            out.push_str(", ");
            out.push_str(entry);
            out.push_str(&text[last_span.end..]);
        }
    }
    Some(out)
}

/// `text` with `array`'s element `at` removed: its whole line, comment and all, when it is the
/// only value on that line, and otherwise the element and the comma beside it. `array` was
/// parsed from `text`, or from a text that differs from it only after that element. `None`
/// when `array` carries no source spans.
#[cfg(unix)]
fn splice_remove(text: &str, array: &aterm_toml::edit::Array, at: usize) -> Option<String> {
    let span = array.get(at)?.span()?;
    let blank = |b: u8| b == b' ' || b == b'\t';
    let bytes = text.as_bytes();
    let skip_blank = |mut pos: usize| {
        while bytes.get(pos).copied().is_some_and(blank) {
            pos += 1;
        }
        pos
    };
    let line = text[..span.start].rfind('\n').map_or(0, |at| at + 1);
    let alone_before = text[line..span.start].bytes().all(blank);
    let mut after = skip_blank(span.end);
    let comma = bytes.get(after) == Some(&b',');
    if comma {
        after = skip_blank(after + 1);
    }
    let mut end_of_line = after;
    if bytes.get(end_of_line) == Some(&b'#') {
        end_of_line += text[end_of_line..]
            .find('\n')
            .unwrap_or(text.len() - end_of_line);
    }
    if bytes.get(end_of_line) == Some(&b'\r') {
        end_of_line += 1;
    }
    let (cut_from, cut_to) = if alone_before && bytes.get(end_of_line) == Some(&b'\n') {
        (line, end_of_line + 1)
    } else if comma {
        (span.start, after)
    } else if let Some(previous) = at.checked_sub(1).and_then(|before| array.get(before)) {
        (previous.span()?.end, span.end)
    } else {
        (span.start, span.end)
    };
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..cut_from]);
    out.push_str(&text[cut_to..]);
    Some(out)
}

/// `load` on unix: agh.toml through `read_private`, of origin `File`. With `honour_loose`
/// (`load_for_host`), each profile row's `legacy_loose` is copied onto its profile; without it
/// (`load`), every profile's is false whatever the row says.
#[cfg(unix)]
fn load_private(
    dirs: &Dirs,
    honour_loose: bool,
) -> std::result::Result<Option<(Validated, AghConfig)>, HostConfigRefusal> {
    let whole = |error| HostConfigRefusal { error, first: None };
    let text = match read_config_in(&dirs.config, CONFIG_FILE, &dirs.home, Some(&dirs.config)) {
        ConfigText::Missing => return Ok(None),
        ConfigText::Refused { error, remedy } => {
            let next = remedy.unwrap_or_else(|| POLICY_CHECK.to_string());
            return Err(whole(invalid(format!("agh.toml: {error}"), next)));
        }
        ConfigText::Io(error) => {
            return Err(whole(invalid(format!("agh.toml: {error}"), POLICY_CHECK)));
        }
        ConfigText::NotUtf8 => {
            return Err(whole(invalid(
                "agh.toml is not UTF-8".to_string(),
                POLICY_CHECK,
            )));
        }
        ConfigText::Text(text) => text,
    };
    let judged = judge(&text, &dirs.home);
    match judged.verdict {
        Ok(mut validated) => {
            validated.origin = ConfigOrigin::File;
            if honour_loose && let Some(parsed) = &judged.parsed {
                for (name, row) in &parsed.profiles {
                    if let Some(profile) = validated.profiles.get_mut(name) {
                        profile.set_legacy_loose(row.legacy_loose);
                    }
                }
            }
            let Some(rows) = judged.parsed else {
                return Err(whole(invalid(
                    "validated config has no parsed rows".to_string(),
                    POLICY_CHECK,
                )));
            };
            Ok(Some((validated, rows)))
        }
        Err(faults) => Err(HostConfigRefusal {
            error: invalid(
                format!(
                    "agh.toml breaks {} rule(s): {}",
                    faults.len(),
                    ConfigFault::list(&faults)
                ),
                POLICY_CHECK,
            ),
            first: faults.into_iter().next(),
        }),
    }
}

/// The verb that prints every fault of agh.toml: the next of each refusal of the file.
pub(crate) const POLICY_CHECK: &str = "agh policy check";

/// Refused, `Sub::Policy(RefusalCode::ConfigInvalid)`, with `why` and `next`.
pub(crate) fn invalid(why: String, next: impl Into<String>) -> crate::error::Error {
    crate::policy::Refusal {
        code: crate::policy::RefusalCode::ConfigInvalid,
        why,
        next: next.into(),
        tried: Vec::new(),
    }
    .into_error(None)
}

/// What reading an agh.toml found.
#[cfg(unix)]
pub(crate) enum ConfigText {
    /// No such file.
    Missing,
    /// `read_private` refused the file: its error, and the remedy when there is one.
    Refused {
        /// `PrivateFileError`'s text.
        error: String,
        /// What a person runs.
        remedy: Option<String>,
    },
    /// The file could not be read (`PrivateFileError::Io`): its text.
    Io(String),
    /// The bytes are not UTF-8.
    NotUtf8,
    /// The text.
    Text(String),
}

/// A parsed agh.toml's row counts (all zero when it does not parse).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Counts {
    /// Owner rows.
    pub(crate) owners: usize,
    /// Profile rows.
    pub(crate) profiles: usize,
    /// Target rows.
    pub(crate) targets: usize,
    /// `[[writers]]` rows.
    pub(crate) writers: usize,
}

/// What [`judge`] found: the parsed file's row counts, the rows as written, and the
/// validated file or its faults.
#[cfg(unix)]
pub(crate) struct Judgement {
    /// The row counts.
    pub(crate) counts: Counts,
    /// The rows as written, when the text parsed: `policy check` reports their `[[writers]]`
    /// and `legacy_loose`, and `load_for_host` copies `legacy_loose` from them.
    pub(crate) parsed: Option<AghConfig>,
    /// The validated file, or its faults.
    pub(crate) verdict: std::result::Result<Validated, Vec<ConfigFault>>,
}

#[cfg(unix)]
impl Judgement {
    /// No rows, and `faults`.
    fn faults(faults: Vec<ConfigFault>) -> Judgement {
        Judgement {
            counts: Counts::default(),
            parsed: None,
            verdict: Err(faults),
        }
    }
}

/// `text` parsed and validated.
#[cfg(unix)]
pub(crate) fn judge(text: &str, home: &Path) -> Judgement {
    match parse(text) {
        Err(fault) => Judgement::faults(vec![fault]),
        Ok(parsed) => Judgement {
            counts: Counts {
                owners: parsed.owners.len(),
                profiles: parsed.profiles.len(),
                targets: parsed.targets.len(),
                writers: parsed.writers.len(),
            },
            verdict: validate(&parsed, home, &file_id),
            parsed: Some(parsed),
        },
    }
}

/// What a read of agh.toml found, as the checks report it (`policy check`, `auth status`).
#[cfg(unix)]
pub(crate) fn faults_of(read: ConfigText, home: &Path) -> Option<Judgement> {
    let fault = |rule, what| Some(Judgement::faults(vec![ConfigFault { rule, what }]));
    match read {
        ConfigText::Missing => None,
        ConfigText::Refused { error, remedy } => fault(
            "file",
            match remedy {
                Some(remedy) => format!("{error}; remedy: {remedy}"),
                None => error,
            },
        ),
        ConfigText::Io(error) => fault("file", error),
        ConfigText::NotUtf8 => fault("L1", "agh.toml is not UTF-8".to_string()),
        ConfigText::Text(text) => Some(judge(&text, home)),
    }
}

#[cfg(unix)]
impl ConfigFault {
    /// `faults` as one text: each `{rule}: {what}`, joined with `; `.
    #[must_use]
    pub(crate) fn list(faults: &[ConfigFault]) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        for (i, f) in faults.iter().enumerate() {
            if i > 0 {
                out.push_str("; ");
            }
            let _ = write!(out, "{}: {}", f.rule, f.what);
        }
        out
    }
}

/// `name` in the agh directory `dir` (`Dirs::config`), by [`read_config_in`]'s rules.
#[cfg(unix)]
pub(crate) fn read_config(dir: &Path, name: &str, home: &Path) -> ConfigText {
    read_config_in(dir, name, home, Some(dir))
}

/// `name` in `dir`, read with `read_private`; remedies write paths under `home` as `~/…`.
#[cfg(unix)]
pub(crate) fn read_config_in(
    dir: &Path,
    name: &str,
    home: &Path,
    agh_dir: Option<&Path>,
) -> ConfigText {
    use crate::identity::{Held, PrivateFix, private_fix, symlink_remedy, tilde};
    use aterm_uds::private_file::{PrivateFileError, read_private};
    let held = Held::Config(agh_dir);
    match read_private(dir, name, CONFIG_MAX) {
        Ok(bytes) => String::from_utf8(bytes).map_or(ConfigText::NotUtf8, ConfigText::Text),
        Err(PrivateFileError::Missing) => ConfigText::Missing,
        Err(error) => {
            let file = dir.join(name);
            let remedy = match &error {
                PrivateFileError::NotPrivate(_) | PrivateFileError::Writable(_) => {
                    match private_fix(dir, &file, home, held) {
                        PrivateFix::Fix(fix) => Some(fix),
                        PrivateFix::NotAFile => {
                            Some(format!("{} must be a regular file", tilde(&file, home)))
                        }
                        PrivateFix::NoChmod => Some(keep_elsewhere(dir, name, home, held)),
                        PrivateFix::Unseen => None,
                    }
                }
                PrivateFileError::Symlink => Some(symlink_remedy(dir, name, home, held)),
                PrivateFileError::Io(_) => return ConfigText::Io(error.to_string()),
                PrivateFileError::Missing | PrivateFileError::Malformed(_) => None,
            };
            ConfigText::Refused {
                error: error.to_string(),
                remedy,
            }
        }
    }
}

/// The agh.toml remedy when no chmod of this user's lifts the refusal (the directory is
/// another user's, or one agh never narrows).
#[cfg(unix)]
fn keep_elsewhere(dir: &Path, name: &str, home: &Path, held: crate::identity::Held<'_>) -> String {
    use crate::identity::{Held, tilde};
    if held.is_agh_dir(dir) {
        use std::os::unix::fs::MetadataExt;
        let dir_text = tilde(dir, home);
        let theirs = std::fs::symlink_metadata(dir)
            .is_ok_and(|meta| meta.uid() != aterm_uds::peer::our_uid());
        return if theirs {
            format!(
                "agh reads {name} only in {dir_text}, which is another user's: its owner, or \
                 root, must make it yours again"
            )
        } else {
            format!(
                "agh reads {name} only in {dir_text}, a directory agh never asks you to \
                 narrow: set XDG_CONFIG_HOME so that agh's directory is a private one of yours"
            )
        };
    }
    let agh_dir = match held {
        Held::Config(agh_dir) => agh_dir,
        Held::Token => None,
    };
    match agh_dir {
        Some(agh_dir) => format!(
            "keep {name} in a directory of yours that only you can open, such as {} (0700), \
             and pass --config with its path",
            tilde(agh_dir, home)
        ),
        None => format!(
            "keep {name} in a directory of yours that only you can open, and pass --config \
             with its path"
        ),
    }
}

/// Tests of the crate-private readers: `Dirs::of`'s roots and `read_config_in`'s remedies for
/// a directory no chmod of this user's changes.
#[cfg(all(test, unix))]
#[path = "config_tests.rs"]
mod tests;
