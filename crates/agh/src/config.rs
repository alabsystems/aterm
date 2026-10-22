// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! agh.toml (design §6.2–§6.3): its rows, the validation L1–L10, the release floor, and where
//! agh's files live.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::identity::{Profile, Source};
use crate::policy::{ChainStep, Tier};
use crate::redact::Redactor;
use crate::slug::Slug;

/// Owners held at release tier whatever agh.toml says. Only ever raises a tier.
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
    /// `aterm_types::dirs::aterm_config_dir()/agh`, `state_dir()/agh` and `home_dir()`; `None`
    /// when any of them is, or is relative.
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
    #[must_use]
    pub fn tokens(&self) -> PathBuf {
        self.config.join("tokens")
    }

    /// `<state>/rate`.
    #[must_use]
    pub fn rate(&self) -> PathBuf {
        self.state.join("rate")
    }

    /// `<config>/agh.toml`.
    #[must_use]
    pub fn config_file(&self) -> PathBuf {
        self.config.join(CONFIG_FILE)
    }
}

/// agh.toml as written. `#[non_exhaustive]`: M2 adds `writers` (`[[writers]]`, design §6.2),
/// which M1's `deny_unknown_fields` refuses as an unknown key.
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
}

/// One `[profiles.NAME]` row. `#[non_exhaustive]`: M2 adds `legacy_loose`, which only
/// AghHost's loader honours (design §6.2), and which M1 refuses as an unknown key.
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
    /// One profile, or `"none"` (the default). A list is read so that L4 can name it.
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
    /// The rule: `L1` to `L10`.
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

/// agh.toml after L1–L10. Opaque: its fields are crate-private and it has no `Default`, so
/// outside agh a `Validated` comes only from `validate` or `load`, and no Router is built over
/// rows the rules did not check (design §6.3). M2 keeps `[[writers]]` inside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Validated {
    pub(crate) profiles: BTreeMap<String, Profile>,
    /// Keyed by the lowercased owner name.
    pub(crate) owners: BTreeMap<String, OwnerRule>,
    pub(crate) default_read: Vec<ChainStep>,
    pub(crate) targets: BTreeMap<String, Slug>,
}

impl Validated {
    /// The profiles, by name.
    #[must_use]
    pub fn profiles(&self) -> &BTreeMap<String, Profile> {
        &self.profiles
    }

    /// The owners' rows, keyed by the lowercased owner name.
    #[must_use]
    pub fn owners(&self) -> &BTreeMap<String, OwnerRule> {
        &self.owners
    }

    /// `[default] read`.
    #[must_use]
    pub fn default_read(&self) -> &[ChainStep] {
        &self.default_read
    }

    /// `[targets]`, by name.
    #[must_use]
    pub fn targets(&self) -> &BTreeMap<String, Slug> {
        &self.targets
    }
}

/// L1: the TOML and its schema.
///
/// The fault names the line and the parser's message, never the offending source line:
/// `aterm_toml`'s `Display` quotes that line, and a token pasted into agh.toml (an unknown
/// `token = "…"` key) would then reach `agh policy check`'s stdout. The message itself
/// passes `unquoted`, because it quotes the value it refused, and an unprefixed token (a
/// legacy classic one) has no shape `Redactor::shapes` knows.
///
/// # Errors
///
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

/// The 1-based line that byte `offset` of `text` is on. End of input is on the last line,
/// as `aterm_toml`'s own locator puts it, never on the empty line after a final newline.
pub(crate) fn line_of(text: &str, offset: usize) -> usize {
    let at = offset.min(text.len());
    let at = if at == text.len() && at > 0 {
        at - 1
    } else {
        at
    };
    // `\n` is ASCII, so counting bytes up to any index counts lines.
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
/// `unknown field `legacy_loose`` still names its key; and any remaining run of
/// `[A-Za-z0-9_]` of `TOKEN_FLOOR` bytes or more becomes `…`.
fn unquoted(message: &str) -> String {
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
    mask_long_words(&out)
}

/// `body` split at its closing `quote` (a `\` escapes the next character inside a double-
/// quoted run): the quoted text and what follows the quote. No closing quote takes it all.
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
fn mask_long_words(text: &str) -> String {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    for c in text.chars() {
        if word(c) {
            run.push(c);
            continue;
        }
        out.push_str(if run.len() >= TOKEN_FLOOR {
            "…"
        } else {
            &run
        });
        run.clear();
        out.push(c);
    }
    out.push_str(if run.len() >= TOKEN_FLOOR {
        "…"
    } else {
        &run
    });
    out
}

/// The chain step that sends no credential.
const ANONYMOUS: &str = "anonymous";
/// An owner row's `write` that names no writer.
const NONE: &str = "none";

/// `[A-Za-z0-9][A-Za-z0-9-]{0,38}`: a GitHub owner name (L3).
pub(crate) fn is_owner_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=39).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
}

/// `[a-z][a-z0-9-]{0,31}`: a profile or target name (L10).
pub(crate) fn is_row_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=32).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// L10's fault for a profile name, or `None` when it is one: `anonymous` and `none` are
/// reserved, and any other name must pass [`is_row_name`]. The one text that `validate` and
/// `auth add` both give.
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

/// The usage error for a profile agh.toml does not name: `no profile named {name}`, with
/// `next` (the verb that lists the profiles there are).
pub(crate) fn no_profile(name: &str, next: &str) -> crate::error::Error {
    crate::error::Error::usage(format!("no profile named {name}")).with_next(next)
}

/// 12 lowercase hex characters: a fingerprint (`identity::fingerprint`).
fn is_fingerprint(text: &str) -> bool {
    text.len() == 12
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Whether `owner` is on the release floor, ASCII case-insensitively.
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
/// stdout and in every verb's refusal of the file. When agh.toml fails nothing has been
/// loaded, so the Redactor has learned no token, and its shapes know only the prefixed
/// forms, while an unprefixed token (a legacy classic one, a GHES one) is still a working
/// credential.
///
/// So a value is shown verbatim when it passes its own name rule (`is_owner_name` for an
/// owner or a login, `is_row_name` for a profile, step, writer or target name) and holds no
/// all-hex run of `TOKEN_FLOOR` bytes or more; every other value has each run of
/// `[A-Za-z0-9_]` of `TOKEN_FLOOR` bytes or more shown as `…`, as on the parse path. No
/// GitHub token passes: a prefixed one holds `_`, which neither rule allows, and a legacy
/// one is 40 hex digits. This masking is beside plan §4.6.12's fault texts, which carry
/// `{o}`, `{p}` and `{t}` verbatim.
struct Shown<'a> {
    value: &'a str,
    rule: Option<fn(&str) -> bool>,
}

impl std::fmt::Display for Shown<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let named = self.rule.is_some_and(|rule| rule(self.value));
        if named && !has_hex_run(self.value) {
            f.write_str(self.value)
        } else {
            f.write_str(&mask_long_words(self.value))
        }
    }
}

/// Whether `text` holds a run of `[A-Za-z0-9_]` of `TOKEN_FLOOR` bytes or more that is all
/// hex digits: what a legacy token, or a part of one, looks like.
fn has_hex_run(text: &str) -> bool {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .any(|run| run.len() >= TOKEN_FLOOR && run.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// An agh.toml value as a valid profile's row prints it (`agh whoami`, `agh auth status`):
/// verbatim, unless it holds an all-hex run of `TOKEN_FLOOR` bytes or more, which a pasted
/// unprefixed token would, and which the Redactor's shapes do not know; then each long run
/// is `…`, as a fault shows it.
pub(crate) fn masked(text: &str) -> String {
    if has_hex_run(text) {
        mask_long_words(text)
    } else {
        text.to_string()
    }
}

/// `[A-Za-z_][A-Za-z0-9_]{0,63}` with no all-hex run of `TOKEN_FLOOR` bytes or more: what a
/// profile's `env` may name (L1). A pasted token is no variable's name.
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
                // The value is never quoted: a token pasted here is what the rule catches.
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
        Some(Validated {
            profiles,
            owners,
            default_read,
            targets,
        })
    }
}

/// L1 (references and values) to L10, every fault at once, in rule order.
///
/// # Errors
///
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

/// Read and validate `<dirs.config>/agh.toml`. No file → `Ok(None)`. Off unix → `Ok(None)`.
///
/// # Errors
///
/// Refused, `Sub::Policy(RefusalCode::ConfigInvalid)`: for a file `read_private` refuses (why
/// `agh.toml: {error}`, next the chmod remedy in `LoadError::remedy`'s form); for non-UTF-8
/// (why `agh.toml is not UTF-8`); for faults (why `agh.toml breaks {n} rule(s): {rule}:
/// {what}; …`, next `agh policy check`).
pub fn load(dirs: &Dirs) -> Result<Option<Validated>> {
    #[cfg(unix)]
    {
        load_private(dirs)
    }
    #[cfg(not(unix))]
    {
        // No permission check off unix: agh.toml is ignored and every read is anonymous.
        let _ = dirs;
        Ok(None)
    }
}

/// `load` on unix: agh.toml through `read_private`.
#[cfg(unix)]
fn load_private(dirs: &Dirs) -> Result<Option<Validated>> {
    let text = match read_config_in(&dirs.config, CONFIG_FILE, &dirs.home, Some(&dirs.config)) {
        ConfigText::Missing => return Ok(None),
        ConfigText::Refused { error, remedy } => {
            let next = remedy.unwrap_or_else(|| POLICY_CHECK.to_string());
            return Err(invalid(format!("agh.toml: {error}"), next));
        }
        ConfigText::Io(error) => {
            return Err(invalid(format!("agh.toml: {error}"), POLICY_CHECK));
        }
        ConfigText::NotUtf8 => {
            return Err(invalid("agh.toml is not UTF-8".to_string(), POLICY_CHECK));
        }
        ConfigText::Text(text) => text,
    };
    match judge(&text, &dirs.home).verdict {
        Ok(validated) => Ok(Some(validated)),
        Err(faults) => Err(invalid(
            format!(
                "agh.toml breaks {} rule(s): {}",
                faults.len(),
                ConfigFault::list(&faults)
            ),
            POLICY_CHECK,
        )),
    }
}

/// The verb that prints every fault of agh.toml: the next of each refusal of the file.
pub(crate) const POLICY_CHECK: &str = "agh policy check";

/// Refused, `Sub::Policy(RefusalCode::ConfigInvalid)`, with `why` and `next`: how the client,
/// `auth add`/`remove` and `auth status` refuse an agh.toml.
pub(crate) fn invalid(why: String, next: impl Into<String>) -> crate::error::Error {
    crate::policy::Refusal {
        code: crate::policy::RefusalCode::ConfigInvalid,
        why,
        next: next.into(),
        tried: Vec::new(),
    }
    .into_error(None)
}

/// What reading an agh.toml found. `load` and `agh policy check` share it. Unix only: off
/// unix there is no permission check, so agh never reads agh.toml there (design §6.6: every
/// read is anonymous).
#[cfg(unix)]
pub(crate) enum ConfigText {
    /// No such file.
    Missing,
    /// `read_private` refused the file: its error, and the remedy when there is one (the
    /// chmods that make it private, or the link to replace).
    Refused {
        /// `PrivateFileError`'s text.
        error: String,
        /// What a person runs.
        remedy: Option<String>,
    },
    /// The file could not be read (`PrivateFileError::Io`): its text. No refusal of the file,
    /// so provisioning answers it as CouldNotRun; load and the checks report it as a refusal.
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
}

/// What [`judge`] found: the parsed file's row counts, and the validated file or its faults.
#[cfg(unix)]
pub(crate) struct Judgement {
    /// The row counts.
    pub(crate) counts: Counts,
    /// The validated file, or its faults.
    pub(crate) verdict: std::result::Result<Validated, Vec<ConfigFault>>,
}

#[cfg(unix)]
impl Judgement {
    /// No rows, and `faults`.
    fn faults(faults: Vec<ConfigFault>) -> Judgement {
        Judgement {
            counts: Counts::default(),
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
            },
            verdict: validate(&parsed, home, &file_id),
        },
    }
}

/// What a read of agh.toml found, as the checks report it (`policy check`, `auth status`):
/// `None` for no file; else [`judge`]'s answer, where a file `read_private` refuses or cannot
/// read is one fault, rule `file` (`{error}; remedy: {remedy}`), and non-UTF-8 bytes are L1.
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
        faults
            .iter()
            .map(|f| format!("{}: {}", f.rule, f.what))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// `name` in the agh directory `dir` (`Dirs::config`), by [`read_config_in`]'s rules: the
/// crate-private contract `auth status` calls (plan §16.5).
#[cfg(unix)]
pub(crate) fn read_config(dir: &Path, name: &str, home: &Path) -> ConfigText {
    read_config_in(dir, name, home, Some(dir))
}

/// `name` in `dir`, read with `read_private` (at most `CONFIG_MAX` bytes); remedies write
/// paths under `home` as `~/…`. `agh_dir` is `Dirs::config` when the caller has one: the
/// remedy never narrows its parent or grandparent (design §6.2). When `dir` is `agh_dir`,
/// the one place agh reads agh.toml from, a remedy never names `--config`; else (`policy
/// check --config PATH`) it names `agh_dir` as the place to keep the file when no chmod of
/// this user's lifts the refusal.
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
                PrivateFileError::NotPrivate(_) => match private_fix(dir, &file, home, held) {
                    PrivateFix::Fix(fix) => Some(fix),
                    PrivateFix::NotAFile => {
                        Some(format!("{} must be a regular file", tilde(&file, home)))
                    }
                    PrivateFix::NoChmod => Some(keep_elsewhere(dir, name, home, held)),
                    PrivateFix::Unseen => None,
                },
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
/// another user's, or one agh never narrows). In the agh directory itself, which agh reads
/// agh.toml from and nowhere else, the way out is that directory made the person's own and
/// private: its owner gives it back, or the aterm config directory moves (XDG_CONFIG_HOME).
/// For any other file (`policy check --config PATH`), keep it in a private directory of the
/// person's own, the agh directory when the caller has one.
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
/// a directory no chmod of this user's changes. In the crate, because the readers are private.
#[cfg(all(test, unix))]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{ConfigText, Dirs, read_config_in};

    /// A relative root (a relative `XDG_CONFIG_HOME`) names no credential store: agh runs with
    /// no config rather than read one under the current directory.
    #[test]
    fn a_relative_root_is_no_dirs() {
        let abs = |p: &str| Some(PathBuf::from(p));
        let dirs = Dirs::of(abs("/c/aterm"), abs("/s"), abs("/h")).expect("absolute roots");
        assert_eq!(dirs.config, Path::new("/c/aterm/agh"));
        assert_eq!(dirs.state, Path::new("/s/agh"));
        assert_eq!(dirs.home, Path::new("/h"));
        assert_eq!(Dirs::of(abs("rel/aterm"), abs("/s"), abs("/h")), None);
        assert_eq!(Dirs::of(abs("/c/aterm"), abs("rel"), abs("/h")), None);
        assert_eq!(Dirs::of(abs("/c/aterm"), abs("/s"), abs("rel")), None);
        assert_eq!(Dirs::of(None, abs("/s"), abs("/h")), None);
    }

    /// A system directory that is root's (`/private/etc` on macOS, where `/etc` is root's link
    /// to it; `/etc` elsewhere), read as the agh directory itself: the remedy names no
    /// `--config`, which no routed verb takes and which changes no file that routes. Skipped
    /// under uid 0, which owns them.
    #[test]
    fn another_users_agh_directory_names_no_config_flag() {
        if aterm_uds::peer::our_uid() == 0 {
            return;
        }
        let home = Path::new("/nonexistent-home");
        for dir in [Path::new("/private/etc"), Path::new("/etc")] {
            if std::fs::symlink_metadata(dir).is_err() {
                continue;
            }
            match read_config_in(dir, "hosts", home, Some(dir)) {
                ConfigText::Refused { error, remedy } => {
                    let remedy = remedy.unwrap_or_default();
                    assert!(!remedy.is_empty(), "{}: {error}", dir.display());
                    assert!(!remedy.contains("--config"), "{}: {remedy}", dir.display());
                }
                ConfigText::Missing
                | ConfigText::Io(_)
                | ConfigText::NotUtf8
                | ConfigText::Text(_) => {
                    panic!(
                        "{}: read_private took a root-owned directory",
                        dir.display()
                    );
                }
            }
        }
    }
}
