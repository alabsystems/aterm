// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a vault entry's name makes it (`docs/DESIGN-claude-setup-token-2026-11-05.md` §3.1; part
//! of one-mesh B13): one compiled classification that the materializer, the CLI and the document
//! diff ask, and the one definition of every name it classes. The consumers of fleet
//! configuration and release material import their names from here.
//!
//! Entry names are ASCII and macOS's file system matches them without case, so names are compared
//! ASCII-folded. Every name this module defines is spelled in lower case; a name whose fold is one
//! of them but whose spelling is not is [`SetApart::CaseVariant`]: on a Mac's disk it is that
//! name's file. A plain name held only by its configuration suffix is no name this module defines,
//! and is held in every spelling.

use std::collections::BTreeMap;

/// The two GitHub tokens: the development one and the release channel's, `alabsystems`' writer.
/// Spelled in `aterm_types::vault_entry`, which agh also reads; it links no aterm-secrets.
pub use aterm_types::vault_entry::{DEV_GH_TOKEN, RELEASE_GH_TOKEN};

/// The notary credential the release subscriber consumes.
pub const NOTARY: &str = "release-notary";

/// The one path `publish/site.toml`'s `firebase_sa_key` resolves to, relative to its `dir`
/// (`crates/aterm-release/src/site/mod.rs`): the entry the site deployer's key arrives as.
pub const SITE_DEPLOYER_KEY: &str =
    "~/company-life/config/local/service-account.alab-site-deployer.json";

/// The release ring's files: its signing key, its machine record and its credentials profile.
pub const RELEASE_RING: [&str; 3] = [
    "~/.aterm/release-ring/machine.key",
    "~/.aterm/release-ring/machine.toml",
    "~/.aterm/release-ring/release-credentials.toml",
];

/// Where the 0.145 vault kept the release ring's files.
pub const LEGACY_RELEASE_RING: [&str; 3] = [
    "~/.aterm/machine.key",
    "~/.aterm/machine.toml",
    "~/.aterm/release-credentials.toml",
];

/// The signed release roster and its detached signature.
pub const PUBLIC_ROSTER: [&str; 2] = [
    "~/.aterm/release-ring/roster/aterm-machines.toml",
    "~/.aterm/release-ring/roster/aterm-machines.toml.sig",
];

/// Where the 0.145 vault kept the signed release roster.
pub const LEGACY_ROSTER: [&str; 2] = [
    "~/.aterm/roster/aterm-machines.toml",
    "~/.aterm/roster/aterm-machines.toml.sig",
];

/// The Developer ID certificate and key leaves: `<prefix><stem>.cer` and `<prefix><stem>.key`.
pub const APPLE_PREFIX: &str = "~/.aterm/apple/devid-";

/// The fleet's Claude Code account roster.
pub const FLEET_ACCOUNTS: &str = "fleet-accounts.toml";

/// The account pool every Mac's harness takes its rows' holders from.
pub const AI_POOL: &str = "ai-pool.toml";

/// The lab's accounting sheet of AI accounts.
pub const AI_ACCOUNTS_CSV: &str = "ai-accounts.csv";

/// The lab registry's phone sheet.
pub const ALAB_PHONES: &str = "alab-phones.csv";

/// The lab registry's computer sheet.
pub const ALAB_COMPUTERS: &str = "alab-computers.csv";

/// The lab registry's email sheet.
pub const ALAB_EMAILS: &str = "alab-emails.csv";

/// The mailbox routes.
pub const AI_MAILBOX: &str = "ai-mailbox.toml";

/// The repository inventory setup clones from.
pub const GITHUB_REPOSITORIES: &str = "github-repositories.json";

/// The mailbox configuration a Mac's sign-in reads after [`AI_MAILBOX`], whose fields fill what
/// it leaves empty and whose routes it adds.
pub const MAILBOX: &str = "mailbox.toml";

/// The sealed per-email sign-in routes and the mailbox URLs warm Macs read.
pub const AI_SIGNIN: &str = "ai-signin.toml";

/// The steward order a sign-in sends its mail-code requests to.
pub const SWARM_POLICY_FILE: &str = "swarm-policy.toml";

/// The native execution assignments every Mac's shared native pool enforces.
pub const AI_NATIVE_POOL: &str = "ai-native-pool.toml";

/// The root-authored proposal metadata the account broker delegates by.
pub const AI_BROKER: &str = "ai-broker.toml";

/// The billing source and account every Mac observes.
pub const AI_BILLING: &str = "ai-billing.toml";

/// What a native pool's root-authored API key allocation is named: this, then 48 hex digits
/// ([`native_api_key`]).
pub const NATIVE_API_KEY_PREFIX: &str = "native-api-key-";

/// What each pooled CLI's fleet file starts and ends with ([`cli_accounts`]).
const CLI_ACCOUNTS: (&str, &str) = ("ai-accounts-", ".toml");

/// The suffixes of a configuration file. A plain name with one of them that this module does not
/// name is configuration some Mac may obey, never a credential leaf a member rotates
/// ([`spelled`]).
const CONFIG_SUFFIXES: [&str; 8] = [
    ".toml", ".json", ".csv", ".yml", ".yaml", ".conf", ".ini", ".plist",
];

/// The fleet file of the pooled CLI whose id is `cli` (`ai-accounts-codex.toml`).
#[must_use]
pub fn cli_accounts(cli: &str) -> String {
    format!("{}{cli}{}", CLI_ACCOUNTS.0, CLI_ACCOUNTS.1)
}

/// The fleet configuration every Mac obeys, by its fixed names ([`cli_accounts`] adds one per
/// pooled CLI, [`native_api_key`] the native pool's allocations, and any other plain name with a
/// configuration suffix is held with them).
pub const FLEET_CONFIGURATION: [&str; 14] = [
    FLEET_ACCOUNTS,
    AI_POOL,
    AI_ACCOUNTS_CSV,
    ALAB_PHONES,
    ALAB_COMPUTERS,
    ALAB_EMAILS,
    AI_MAILBOX,
    GITHUB_REPOSITORIES,
    MAILBOX,
    AI_SIGNIN,
    SWARM_POLICY_FILE,
    AI_NATIVE_POOL,
    AI_BROKER,
    AI_BILLING,
];

/// git's configuration, whose credential helper runs on every Mac that receives it.
pub const GIT_CONFIG: &str = "~/.config/git/config";

/// gh's hosts file.
pub const GH_HOSTS: &str = "~/.config/gh/hosts.yml";

/// git's stored credentials.
pub const GIT_CREDENTIALS: &str = "~/.config/git/credentials";

/// The fleet code.
pub const FLEET_CODE: &str = "aterm-fleet-code";

/// What a Claude Code token entry's name starts with: `aterm-claude-token-ac-<16 hex>`, and its
/// value-free record beside it, the same name and `-meta` (DESIGN-claude-setup-token §4.1).
pub const CLAUDE_TOKEN_PREFIX: &str = "aterm-claude-token-";

/// What a Claude Code token's record adds to the token's name.
pub const CLAUDE_TOKEN_META: &str = "-meta";

/// What a vault entry is, by its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryClass {
    /// A file members rotate: a credential leaf below `~/.secrets` (a plain name with no
    /// configuration suffix that this module does not name), gh's hosts file or git's credentials.
    Shared,
    /// Materialized as today, but out of a member write's reach.
    Held(Held),
    /// Read in place from the encrypted vault cache, never a file.
    Consumed(Consumed),
    /// Never written and never taken back.
    SetApart(SetApart),
}

/// Why an entry is [`EntryClass::Held`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Held {
    /// Release material: it stays as it stands until it leaves for the release Macs (one-mesh
    /// §4.9, A14).
    Release,
    /// Fleet configuration every Mac obeys, or a root-authored record it reads (a native pool's
    /// API key allocation), until B13 gives it typed writers.
    FleetConfiguration,
    /// git's configuration, which runs code on every Mac, until B13's compiled writer.
    GitConfig,
}

/// Why an entry is [`EntryClass::Consumed`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Consumed {
    /// A control record every Mac obeys: the fleet code, the swarm roster, policy and lease, the
    /// Commons control.
    Control,
    /// A Claude Code token or its record, which only aterm's token call writes.
    ClaudeToken,
}

/// Why an entry is [`EntryClass::SetApart`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetApart {
    /// An AI CLI's login file ([`crate::text::cli_login`]).
    CliLogin,
    /// A home path aterm does not name.
    HomePath,
    /// Another spelling of a [`Held`] or [`Consumed`] name this module names, which is that
    /// name's file on disk.
    CaseVariant,
    /// A name in the Claude Code token namespace outside the token's grammar.
    TokenNamespace,
}

impl EntryClass {
    /// Whether the materializer writes the entry as a file.
    #[must_use]
    pub const fn materialized(self) -> bool {
        matches!(self, Self::Shared | Self::Held(_))
    }
}

/// The class of the entry `name`.
#[must_use]
pub fn of(name: &str) -> EntryClass {
    if name.starts_with("~/") && crate::text::cli_login(name).is_some() {
        return EntryClass::SetApart(SetApart::CliLogin);
    }
    let fold = fold(name);
    if fold.starts_with(CLAUDE_TOKEN_PREFIX) && !claude_token(&fold) {
        return EntryClass::SetApart(SetApart::TokenNamespace);
    }
    match spelled(&fold) {
        Some(class) if fold == name => class,
        // Two spellings of one shared file name are both names, and so are two of a plain name
        // held only by its configuration suffix, which names no file a consumer reads: the
        // folded-pair check ([`folded_pairs`]) keeps them from landing on one file.
        Some(EntryClass::Shared) if !name.starts_with("~/") => EntryClass::Shared,
        Some(class @ EntryClass::Held(Held::FleetConfiguration)) if suffix_held(&fold) => class,
        Some(EntryClass::Shared) | None => EntryClass::SetApart(SetApart::HomePath),
        Some(_) => EntryClass::SetApart(SetApart::CaseVariant),
    }
}

/// Whether the lower-case `fold` is held only by its configuration suffix ([`CONFIG_SUFFIXES`]):
/// a plain name this module does not name. No consumer reads such a file by name, so every
/// spelling of it is held alike, never a case variant of a name every Mac obeys.
fn suffix_held(fold: &str) -> bool {
    !fold.starts_with("~/")
        && CONFIG_SUFFIXES.iter().any(|end| fold.ends_with(end))
        && !FLEET_CONFIGURATION.contains(&fold)
        && !pooled_cli_accounts(fold)
        && !native_api_key(fold)
}

/// `name` as a Mac's disk compares it: ASCII-folded.
fn fold(name: &str) -> String {
    #[cfg(test)]
    if UNFOLDED.with(std::cell::Cell::get) {
        return name.to_owned();
    }
    name.to_ascii_lowercase()
}

#[cfg(test)]
thread_local! {
    /// The negative control of the fold, on for one test thread: names are compared byte for
    /// byte, as before the classification, so a case variant of a held name is a shared name of
    /// its own and lands on that name's file.
    pub(crate) static UNFOLDED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The class a lower-case `fold` outside the token namespace's other names is spelled as: a
/// plain name this module does not name is [`EntryClass::Shared`] where it is a credential leaf
/// and fleet configuration where it carries a configuration suffix, and a home path it does not
/// name is `None`.
fn spelled(fold: &str) -> Option<EntryClass> {
    if claude_token(fold) {
        return Some(EntryClass::Consumed(Consumed::ClaudeToken));
    }
    if control(fold) {
        return Some(EntryClass::Consumed(Consumed::Control));
    }
    if fold == RELEASE_GH_TOKEN
        || fold == NOTARY
        || fold == SITE_DEPLOYER_KEY
        || RELEASE_RING.contains(&fold)
        || LEGACY_RELEASE_RING.contains(&fold)
        || PUBLIC_ROSTER.contains(&fold)
        || LEGACY_ROSTER.contains(&fold)
        || signing_material(fold)
    {
        return Some(EntryClass::Held(Held::Release));
    }
    if FLEET_CONFIGURATION.contains(&fold)
        || pooled_cli_accounts(fold)
        || native_api_key(fold)
        || suffix_held(fold)
    {
        return Some(EntryClass::Held(Held::FleetConfiguration));
    }
    if fold == GIT_CONFIG {
        return Some(EntryClass::Held(Held::GitConfig));
    }
    if fold == GH_HOSTS || fold == GIT_CREDENTIALS || !fold.starts_with("~/") {
        return Some(EntryClass::Shared);
    }
    None
}

/// Whether `name` is a native pool's root-authored API key allocation:
/// `native-api-key-<48 hex>`.
#[must_use]
pub fn native_api_key(name: &str) -> bool {
    name.strip_prefix(NATIVE_API_KEY_PREFIX)
        .is_some_and(|hex| hex.len() == 48 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Whether `name` is a control record's name as the vault spells it.
fn control(name: &str) -> bool {
    name == FLEET_CODE
        || name == crate::SWARM_ROSTER_ENTRY
        || name == crate::SWARM_POLICY_ENTRY
        || name == crate::SWARM_LEASE_ENTRY
        || name.starts_with(crate::commons::CONTROL_PREFIX)
}

/// Whether `name` is a pooled CLI's fleet file ([`cli_accounts`]).
fn pooled_cli_accounts(name: &str) -> bool {
    name.strip_prefix(CLI_ACCOUNTS.0)
        .and_then(|rest| rest.strip_suffix(CLI_ACCOUNTS.1))
        .is_some_and(|cli| !cli.is_empty())
}

/// Whether `name` is a Developer ID certificate or key leaf (`~/.aterm/apple/devid-<stem>.cer`
/// or `.key`, the stem one path component).
#[must_use]
pub fn signing_material(name: &str) -> bool {
    name.strip_prefix(APPLE_PREFIX)
        .and_then(|leaf| {
            leaf.strip_suffix(".cer")
                .or_else(|| leaf.strip_suffix(".key"))
        })
        .is_some_and(|stem| !stem.is_empty() && !stem.contains(['/', '\\']))
}

/// Whether `name` is a Claude Code token entry or its record, spelled exactly:
/// `aterm-claude-token-ac-[0-9a-f]{16}(-meta)?`.
#[must_use]
pub fn claude_token(name: &str) -> bool {
    let Some(key) = name.strip_prefix(CLAUDE_TOKEN_PREFIX) else {
        return false;
    };
    let key = key.strip_suffix(CLAUDE_TOKEN_META).unwrap_or(key);
    key.strip_prefix("ac-").is_some_and(|hex| {
        hex.len() == 16
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// Whether `name`, in any spelling, is consumed in place and never becomes a file: a
/// [`Consumed`] name or a case variant of one, also as a `~/.secrets/` path.
#[must_use]
pub fn reserved(name: &str) -> bool {
    let fold = name.to_ascii_lowercase();
    let bare = fold.strip_prefix("~/.secrets/").unwrap_or(&fold);
    claude_token(bare) || control(bare)
}

/// Each name of `names` the materializer writes that shares its fold with another such name,
/// with the first other: on a Mac's disk the two are one file, so neither is written.
#[must_use]
pub fn folded_pairs<'a>(names: impl IntoIterator<Item = &'a str>) -> BTreeMap<&'a str, &'a str> {
    let mut by_fold: BTreeMap<String, Vec<&'a str>> = BTreeMap::new();
    for name in names {
        if of(name).materialized() {
            let spellings = by_fold.entry(fold(name)).or_default();
            if !spellings.contains(&name) {
                spellings.push(name);
            }
        }
    }
    let mut pairs = BTreeMap::new();
    for spellings in by_fold.values().filter(|spellings| spellings.len() > 1) {
        for &name in spellings {
            if let Some(&other) = spellings.iter().find(|other| **other != name) {
                pairs.insert(name, other);
            }
        }
    }
    pairs
}

/// Why `name` and `other`, which fold alike, are refused.
#[must_use]
pub fn folded_pair_refusal(name: &str, other: &str) -> String {
    format!("`{name}` and `{other}` are one file on a Mac's disk; unset `{other}` first")
}

/// Why no Mac writes the entry `name` as a file, where it is [`EntryClass::SetApart`].
#[must_use]
pub fn set_apart_refusal(name: &str) -> Option<String> {
    let EntryClass::SetApart(why) = of(name) else {
        return None;
    };
    Some(match why {
        SetApart::CliLogin => crate::text::cli_login_refusal(name).unwrap_or_default(),
        SetApart::HomePath => format!(
            "`{name}` is a home path no vault writes: a Mac takes ~/.secrets files from the vault, \
             and of home paths only gh's hosts.yml, git's config and credentials and the release \
             material aterm names"
        ),
        SetApart::CaseVariant => {
            let fold = fold(name);
            let what = match spelled(&fold) {
                Some(EntryClass::Held(Held::Release)) => "release material",
                Some(EntryClass::Held(Held::FleetConfiguration)) => {
                    "fleet configuration every Mac obeys"
                }
                Some(EntryClass::Held(Held::GitConfig)) => "git's configuration",
                Some(EntryClass::Consumed(Consumed::ClaudeToken)) => "a Claude Code token",
                _ => "a control record every Mac obeys",
            };
            format!("`{name}` is `{fold}` on a Mac's disk, and that is {what}")
        }
        SetApart::TokenNamespace => format!(
            "`{name}` is in the Claude Code token namespace ({CLAUDE_TOKEN_PREFIX}ac-<16 hex>), \
             which no vault text writes"
        ),
    })
}

/// Why a member's write (`aterm secrets set` on a seated Mac, `client::member_set`) refuses the
/// entry `name` (DESIGN-claude-setup-token §3.3): only a [`EntryClass::Shared`] name is a
/// member's to write. `None` for a shared name.
#[must_use]
pub fn member_refusal(name: &str) -> Option<String> {
    if !crate::text::valid_entry_name(name) {
        return Some(format!(
            "{name:?} is not a valid name (letters, digits, . _ -; or ~/a/b)"
        ));
    }
    Some(match of(name) {
        EntryClass::Shared => return None,
        EntryClass::Consumed(Consumed::Control) => {
            format!("`{name}` is a control record every Mac obeys: no member write changes it")
        }
        EntryClass::Consumed(Consumed::ClaudeToken) => claude_token_refusal(name),
        EntryClass::Held(Held::Release) => format!(
            "`{name}` is release material, kept as it stands until it leaves for the release Macs \
             (one-mesh §4.9): no member write changes it"
        ),
        EntryClass::Held(Held::FleetConfiguration) => format!(
            "`{name}` is fleet configuration every Mac obeys; until B13 gives it a typed writer, \
             an admin's `set` changes it"
        ),
        EntryClass::Held(Held::GitConfig) => {
            format!("`{name}` runs code on every Mac; B13 replaces it with aterm's own writer")
        }
        EntryClass::SetApart(_) => set_apart_refusal(name)?,
    })
}

/// Why no vault text writes the entry `name` (an admin's `set`, `edit` or `create --import`):
/// it is set apart, or it is a Claude Code token, which only aterm's token call writes, into the
/// vault's log (DESIGN-claude-setup-token §4.4).
#[must_use]
pub fn text_refusal(name: &str) -> Option<String> {
    if of(name) == EntryClass::Consumed(Consumed::ClaudeToken) {
        return Some(claude_token_refusal(name));
    }
    set_apart_refusal(name)
}

/// Why a vault text never holds the Claude Code token entry `name`.
#[must_use]
pub fn claude_token_refusal(name: &str) -> String {
    format!(
        "`{name}` is a Claude Code token, which only aterm's own token call writes, into the \
         vault's log; no vault text holds one"
    )
}

/// Which check of a stored Claude token failed (§4.3). It is declared here, not beside the
/// Unix-only reader that finds it, so a surface on any platform can name the state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mismatch {
    /// The two names' winning `Put`s are not one draft, a name is conflicted, or one is absent.
    Draft,
    /// A value is not inner-sealed, the token is not a setup-token, or the meta does not parse.
    Value,
}

#[cfg(test)]
#[path = "entry_class_tests.rs"]
mod tests;
