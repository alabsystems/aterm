// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WANT CATALOG (docs/DESIGN-credential-requests-2026-10-27.md §4), compiled in: what a Mac
//! may ask the lab for, where copies of each live by design, who can make a new one, the
//! entry it arrives as, the hold probe a holder runs (metadata only), and why a want that is
//! never asked is never asked. No record and no admin can make a want askable that this file
//! does not.

/// The vault entry the site deployer's key arrives as ([`crate::entry_class::SITE_DEPLOYER_KEY`]).
pub const SITE_DEPLOYER_KEY_ENTRY: &str = crate::entry_class::SITE_DEPLOYER_KEY;

/// The notary credential consumed by the release subscriber ([`crate::entry_class::NOTARY`]).
/// The legacy `notary` entry triggers older clients before their developer-tools checks, so it
/// is never supplied.
pub const NOTARY_ENTRY: &str = crate::entry_class::NOTARY;

/// The vault entry the release roster's paper master phrase arrives as
/// ([`crate::entry_class::PAPER_MASTER`]).
pub const PAPER_MASTER_ENTRY: &str = crate::entry_class::PAPER_MASTER;

/// The vault entry the Azure signing principal arrives as: its `AZURE_TENANT_ID`,
/// `AZURE_CLIENT_ID` and `AZURE_CLIENT_SECRET` lines, which `aterm-winsign` hands to signtool.
pub const AZURE_SIGNING_PRINCIPAL_ENTRY: &str = "azure_signing_principal";

/// The catalog's version, cited as a catalog retirement's evidence.
pub const VERSION: u32 = 3;

/// The most characters a mailbox label or a `login:` name takes.
const MAX_LABEL: usize = 63;

/// Which GitHub token a `gh:` want names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GhProfile {
    /// `gh_access_token`, agh's dev identity.
    Dev,
    /// `gh_access_token_alabsystems`, the release identity.
    Release,
    /// `gh_access_token_fleet`, the machine user `aterm machines` registers hosts' keys with.
    Fleet,
}

/// The vault entry the fleet machine user's GitHub token arrives as.
pub const FLEET_GH_TOKEN_ENTRY: &str = "gh_access_token_fleet";

/// The vault entry `aterm machines enroll` signs in to a Mac's admin account with.
pub const MACHINES_ADMIN_ENTRY: &str = "machines_admin_password";

/// The kind word of a Claude Code token want: `claude-token:<key>`.
const CLAUDE_TOKEN: &str = "claude-token";

/// The one verb that makes a Claude Code token ([`Want::producer`]): a person signs in to the
/// account in a browser, and the verb stores the token in the vault's log
/// (docs/DESIGN-claude-setup-token-2026-11-05.md §4.6).
pub const CLAUDE_TOKEN_PRODUCER: &str = "aterm harness accounts token <label|email>";

/// What a request names: never a file path a record could choose.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Want {
    /// `site-deployer-key`: the site deployer's service-account key.
    SiteDeployerKey,
    /// `notary`: the notary credential, the vault entry `release-notary`.
    Notary,
    /// `developer-id:<stem>`: a Developer ID Application certificate and its key, the vault
    /// entries `~/.aterm/apple/devid-<stem>.cer` and `.key`.
    DeveloperId(String),
    /// `mailbox:<label>`: one AI account's mailbox password (and TOTP seed).
    Mailbox(String),
    /// One credential of a registered mailbox, bound to its authority and login.
    MailboxComponent(crate::mailbox_component::Component),
    /// `gh:<dev|release|fleet>`: a vault GitHub token.
    Gh(GhProfile),
    /// `entry:<name>`: any vault entry by name, answered from this Mac's own envelope.
    Entry(String),
    /// `paper-master`: the release roster's paper master phrase.
    PaperMaster,
    /// `login:<provider>/<label>`: a vendor login.
    Login(String),
    /// `browser-profile:<label>`: a row's browser profile.
    BrowserProfile(String),
    /// `refresh-token:<provider>/<label>`: a vendor refresh token.
    RefreshToken(String),
    /// `vault-identity`: this Mac's own vault age identity.
    VaultIdentity,
    /// `commons-device-key`: this Mac's Commons writer key.
    CommonsDeviceKey,
    /// `commons-control`: the Commons control entry.
    CommonsControl,
    /// `machines-admin`: the admin password `aterm machines enroll` signs in to a Mac with.
    MachinesAdmin,
    /// `claude-token:<key>`: the Claude Code token of the account `<key>` (`ac-<16 hex>`), the
    /// `[everyone]` entry `aterm-claude-token-<key>` of the vault's log. It has one producer
    /// ([`Want::producer`]) and lives only in the log, inner-sealed: no Mac gives or copies it,
    /// and a request for it ends once the log gives the requester a token in use there.
    ClaudeToken(crate::claude_token::Key),
    /// `azure-signing-principal`: the Azure service principal Windows images are signed as.
    AzureSigningPrincipal,
    /// `fleet-code`: the default ALab fleet's join code, the vault's `[everyone]` control entry
    /// ([`crate::client::FLEET_CODE_ENTRY`]) an ALab admin publishes (`aterm fleet publish`).
    FleetCode,
}

/// Where a want's copies live by design, which decides whether it is ever posted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Copies live on Macs' disks (release Macs): posted, and holders may offer one.
    Copyable,
    /// Copies live in the vault by design: posted; a stray copy may still be offered, except of a
    /// want with a [`Want::producer`], which no Mac offers.
    VaultOnly,
    /// Each Mac makes its own; none is ever copied.
    PerMac,
    /// An act of the vault's admin, not a value anyone holds.
    AdminAct,
}

impl Class {
    /// Its word in `need ls`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Copyable => "copyable",
            Self::VaultOnly => "vault-only",
            Self::PerMac => "per-mac",
            Self::AdminAct => "admin-act",
        }
    }
}

fn valid_label(label: &str) -> bool {
    let bytes = label.as_bytes();
    (1..=MAX_LABEL).contains(&bytes.len())
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes.iter().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'.' | b'_')
        })
}

fn valid_provider_label(word: &str) -> bool {
    word.split_once('/')
        .is_some_and(|(provider, label)| valid_label(provider) && valid_label(label))
}

impl Want {
    /// The want `word` spells ([`Want::key`]).
    ///
    /// # Errors
    /// A word the catalog does not name, or a label or entry name that is not valid.
    pub fn parse(word: &str) -> Result<Self, String> {
        let bad = |what: &str| {
            Err(format!(
                "{word:?}: {what} (wants: site-deployer-key, notary, developer-id:<stem>, mailbox:<label>, mailbox-field:<identity>/<component>, gh:dev, \
                 gh:release, gh:fleet, machines-admin, claude-token:<ac-key>, entry:<name>, paper-master, login:<provider>/<label>, \
                 browser-profile:<label>, refresh-token:<provider>/<label>, vault-identity, \
                 commons-device-key, commons-control, azure-signing-principal, fleet-code)"
            ))
        };
        let want = match word {
            "site-deployer-key" => Self::SiteDeployerKey,
            "notary" => Self::Notary,
            "paper-master" => Self::PaperMaster,
            "vault-identity" => Self::VaultIdentity,
            "commons-device-key" => Self::CommonsDeviceKey,
            "commons-control" => Self::CommonsControl,
            "machines-admin" => Self::MachinesAdmin,
            "gh:fleet" => Self::Gh(GhProfile::Fleet),
            "azure-signing-principal" => Self::AzureSigningPrincipal,
            "fleet-code" => Self::FleetCode,
            "gh:dev" => Self::Gh(GhProfile::Dev),
            "gh:release" => Self::Gh(GhProfile::Release),
            _ => {
                let Some((kind, rest)) = word.split_once(':') else {
                    return bad("not a want the catalog names");
                };
                match kind {
                    "mailbox-field" => Self::MailboxComponent(
                        crate::mailbox_component::Component::parse(word).ok_or_else(|| {
                            "invalid mailbox component identity or part".to_owned()
                        })?,
                    ),
                    "mailbox" if valid_label(rest) => Self::Mailbox(rest.to_owned()),
                    "developer-id" if valid_label(rest) => Self::DeveloperId(rest.to_owned()),
                    "entry"
                        if crate::text::valid_entry_name(rest) && !crate::reserved_name(rest) =>
                    {
                        Self::Entry(rest.to_owned())
                    }
                    "login" if valid_provider_label(rest) => Self::Login(rest.to_owned()),
                    "refresh-token" if valid_provider_label(rest) => {
                        Self::RefreshToken(rest.to_owned())
                    }
                    "browser-profile" if valid_label(rest) => Self::BrowserProfile(rest.to_owned()),
                    CLAUDE_TOKEN => match crate::claude_token::Key::parse(rest) {
                        Some(key) => Self::ClaudeToken(key),
                        None => {
                            return bad(
                                "claude-token: takes an account key, ac-<16 lower-case hex> (the <key> of the vault entry aterm-claude-token-<key>)",
                            );
                        }
                    },
                    "mailbox" | "developer-id" | "login" | "refresh-token" | "browser-profile" => {
                        return bad("not a valid label");
                    }
                    "entry" if crate::entry_class::claude_token(rest) => {
                        return bad("a Claude Code token is asked for as claude-token:<ac-key>");
                    }
                    "entry" => return bad("not a valid vault entry name"),
                    "gh" => return bad("gh: takes dev, release or fleet"),
                    _ => return bad("not a want the catalog names"),
                }
            }
        };
        Ok(want)
    }

    /// Its one spelling: what a record, the ledger and every `NEEDS YOU` line name.
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Self::SiteDeployerKey => "site-deployer-key".to_owned(),
            Self::Notary => "notary".to_owned(),
            Self::DeveloperId(stem) => format!("developer-id:{stem}"),
            Self::Mailbox(label) => format!("mailbox:{label}"),
            Self::MailboxComponent(component) => component.key(),
            Self::Gh(GhProfile::Dev) => "gh:dev".to_owned(),
            Self::Gh(GhProfile::Release) => "gh:release".to_owned(),
            Self::Gh(GhProfile::Fleet) => "gh:fleet".to_owned(),
            Self::Entry(name) => format!("entry:{name}"),
            Self::PaperMaster => "paper-master".to_owned(),
            Self::Login(name) => format!("login:{name}"),
            Self::BrowserProfile(label) => format!("browser-profile:{label}"),
            Self::RefreshToken(name) => format!("refresh-token:{name}"),
            Self::VaultIdentity => "vault-identity".to_owned(),
            Self::CommonsDeviceKey => "commons-device-key".to_owned(),
            Self::CommonsControl => "commons-control".to_owned(),
            Self::MachinesAdmin => "machines-admin".to_owned(),
            Self::ClaudeToken(key) => format!("{CLAUDE_TOKEN}:{}", key.as_str()),
            Self::AzureSigningPrincipal => "azure-signing-principal".to_owned(),
            Self::FleetCode => "fleet-code".to_owned(),
        }
    }

    /// Where its copies live by design.
    #[must_use]
    pub const fn class(&self) -> Class {
        match self {
            Self::SiteDeployerKey | Self::Notary => Class::Copyable,
            Self::DeveloperId(_)
            | Self::Mailbox(_)
            | Self::MailboxComponent(_)
            | Self::Gh(_)
            | Self::Entry(_)
            | Self::MachinesAdmin
            | Self::PaperMaster
            | Self::ClaudeToken(_)
            | Self::AzureSigningPrincipal
            | Self::FleetCode => Class::VaultOnly,
            Self::Login(_)
            | Self::BrowserProfile(_)
            | Self::RefreshToken(_)
            | Self::VaultIdentity
            | Self::CommonsDeviceKey => Class::PerMac,
            Self::CommonsControl => Class::AdminAct,
        }
    }

    /// Whether a request for it is ever posted to the lab: copyable wants and the vault-only
    /// `developer-id:*`, `mailbox:*`, `mailbox-field:*`, `gh:*`, `machines-admin`, `paper-master`,
    /// `azure-signing-principal` and `claude-token:*`. `entry:<name>` and `fleet-code` are answered
    /// from this Mac's own envelope.
    #[must_use]
    pub const fn posted(&self) -> bool {
        matches!(
            self,
            Self::SiteDeployerKey
                | Self::Notary
                | Self::DeveloperId(_)
                | Self::Mailbox(_)
                | Self::MailboxComponent(_)
                | Self::Gh(_)
                | Self::MachinesAdmin
                | Self::PaperMaster
                | Self::ClaudeToken(_)
                | Self::AzureSigningPrincipal
        )
    }

    /// The one verb that makes its value, for a want no Mac gives or copies: a Claude Code token
    /// is made by a person's sign-in and lives only in the vault's log, so a request for it is
    /// answered once the log gives the requester a value in use there ([`super::settle`]), never
    /// by a holder's offer, a `need give` or the custodian's copy. `None` for every other want.
    #[must_use]
    pub const fn producer(&self) -> Option<&'static str> {
        match self {
            Self::ClaudeToken(_) => Some(CLAUDE_TOKEN_PRODUCER),
            _ => None,
        }
    }

    /// Why `need give <word>` is refused before it reads anything: `word`, a want or a request
    /// id, names a want with a [`Want::producer`]. A `claude-token:` word is refused whether or not
    /// its key parses. `None` for any other word.
    #[must_use]
    pub fn give_refusal(word: &str) -> Option<String> {
        let (what, producer) = match Self::parse(word) {
            Ok(want) => (want.title(), want.producer()?),
            Err(_) => {
                let (kind, _) = word.split_once(':')?;
                (kind == CLAUDE_TOKEN)
                    .then(|| ("a Claude Code token".to_owned(), CLAUDE_TOKEN_PRODUCER))?
            }
        };
        Some(format!(
            "{what} is never given: its one producer is {producer}, which stores it in the vault's \
             log, inner-sealed, where every member reads it in place; a request for it ends once \
             one in use there reaches the requester"
        ))
    }

    /// The vault entries it arrives as, the first the one a request is answered by.
    #[must_use]
    pub fn entries(&self) -> Vec<String> {
        match self {
            Self::SiteDeployerKey => vec![SITE_DEPLOYER_KEY_ENTRY.to_owned()],
            Self::Notary => vec![NOTARY_ENTRY.to_owned()],
            Self::DeveloperId(stem) => [".cer", ".key"]
                .map(|leaf| format!("{}{stem}{leaf}", crate::entry_class::APPLE_PREFIX))
                .to_vec(),
            Self::Mailbox(label) => vec![format!("{label}_password"), format!("{label}_totp")],
            Self::MailboxComponent(component) => vec![component.entry()],
            Self::Gh(GhProfile::Dev) => vec![crate::entry_class::DEV_GH_TOKEN.to_owned()],
            Self::Gh(GhProfile::Release) => vec![crate::entry_class::RELEASE_GH_TOKEN.to_owned()],
            Self::Gh(GhProfile::Fleet) => vec![FLEET_GH_TOKEN_ENTRY.to_owned()],
            Self::MachinesAdmin => vec![MACHINES_ADMIN_ENTRY.to_owned()],
            Self::ClaudeToken(key) => vec![key.entry(), key.meta_entry()],
            Self::Entry(name) => vec![name.clone()],
            Self::CommonsControl => vec![crate::commons::ENTRY.to_owned()],
            Self::AzureSigningPrincipal => vec![AZURE_SIGNING_PRINCIPAL_ENTRY.to_owned()],
            Self::FleetCode => vec![crate::client::FLEET_CODE_ENTRY.to_owned()],
            Self::PaperMaster => vec![PAPER_MASTER_ENTRY.to_owned()],
            Self::Login(_)
            | Self::BrowserProfile(_)
            | Self::RefreshToken(_)
            | Self::VaultIdentity
            | Self::CommonsDeviceKey => Vec::new(),
        }
    }

    /// The entry whose arrival answers it: the first of [`Want::entries`].
    #[must_use]
    pub fn entry(&self) -> Option<String> {
        self.entries().into_iter().next()
    }

    /// Whether a delivery writes it only where the Mac has no file of its own (`if-absent`).
    #[must_use]
    pub const fn if_absent(&self) -> bool {
        matches!(
            self,
            Self::SiteDeployerKey | Self::Notary | Self::DeveloperId(_)
        )
    }

    /// What it is, in a sentence's words.
    #[must_use]
    pub fn title(&self) -> String {
        match self {
            Self::SiteDeployerKey => "the site deployer's service-account key".to_owned(),
            Self::Notary => "the notary credential".to_owned(),
            Self::DeveloperId(stem) => format!("the Developer ID identity devid-{stem}"),
            Self::Mailbox(label) => format!("{label}'s mailbox password"),
            Self::MailboxComponent(component) => format!("the {}", component.part().description()),
            Self::Gh(GhProfile::Dev) => "the dev GitHub token".to_owned(),
            Self::Gh(GhProfile::Release) => "the release GitHub token".to_owned(),
            Self::Gh(GhProfile::Fleet) => "the fleet machine user's GitHub token".to_owned(),
            Self::MachinesAdmin => {
                "the admin password aterm machines enroll signs in with".to_owned()
            }
            Self::ClaudeToken(key) => {
                format!("the Claude Code token of account {}", key.as_str())
            }
            Self::Entry(name) => format!("the vault entry {name}"),
            Self::PaperMaster => "the release roster's paper master phrase".to_owned(),
            Self::Login(name) => format!("the vendor login {name}"),
            Self::BrowserProfile(label) => format!("{label}'s browser profile"),
            Self::RefreshToken(name) => format!("the refresh token of {name}"),
            Self::VaultIdentity => "this Mac's vault identity".to_owned(),
            Self::CommonsDeviceKey => "this Mac's Commons device key".to_owned(),
            Self::CommonsControl => "the Commons control entry".to_owned(),
            Self::AzureSigningPrincipal => {
                "the Azure service principal Windows images are signed as".to_owned()
            }
            Self::FleetCode => "the default ALab fleet's join code".to_owned(),
        }
    }

    /// The retirement the catalog answers at once, before anything leaves this Mac: its
    /// reason and the explanation `need ls` prints. `None` for a want that may be posted, for
    /// `entry:<name>` (answered from the envelope) and for the admin act (a local wait).
    #[must_use]
    pub fn catalog_retirement(&self) -> Option<(super::Reason, String)> {
        use super::Reason;
        match self.class() {
            Class::PerMac => Some((
                Reason::NeverCopied,
                match self {
                    Self::VaultIdentity | Self::CommonsDeviceKey => format!(
                        "{} is this Mac's own key: a copy on another Mac would make that Mac this one",
                        self.title()
                    ),
                    _ => format!(
                        "{} is made fresh on each Mac by the sign-in ladder, which never copies, stores or relays a login (aterm harness accounts)",
                        self.title()
                    ),
                },
            )),
            Class::Copyable | Class::VaultOnly | Class::AdminAct => None,
        }
    }

    /// The hold probe's one entry: a holder offers only what this names, and the probe asks this
    /// Mac's store whether it holds that entry ([`crate::client::registered_names`]), never a
    /// file and never the value. `None` for a want no holder can offer, a want with a
    /// [`Want::producer`] among them.
    #[must_use]
    pub fn hold_entry(&self) -> Option<String> {
        match self {
            Self::SiteDeployerKey => Some(SITE_DEPLOYER_KEY_ENTRY.to_owned()),
            Self::Notary => Some(NOTARY_ENTRY.to_owned()),
            Self::Mailbox(label) => Some(format!("{label}_password")),
            Self::MailboxComponent(component) => Some(component.entry()),
            _ => None,
        }
    }
}

/// A compiled consumer: who raised a need, and so whose own check says when it is met.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Purpose {
    /// `ship provision`'s `release.site-key` row.
    ReleaseSiteKey,
    /// `ship provision`'s notary row.
    ReleaseNotary,
    /// `ship provision`'s Apple Developer ID row.
    ReleaseDeveloperId,
    /// An AI account's unattended sign-in: its email-code route's mailbox, or its Claude Code
    /// token (`claude-token:`), which the harness asks for where it offers one
    /// ([`super::ask_for_tokens`]).
    HarnessSignIn,
    /// The default repository consumer's development GitHub identity.
    AghDev,
    /// agh's release identity.
    AghRelease,
    /// Commons readiness.
    Commons,
    /// `aterm machines`' host key registration with the fleet machine user.
    MachinesKeys,
    /// `aterm machines enroll`'s admin sign-in.
    MachinesEnroll,
    /// `aterm-winsign sign`'s Azure Artifact Signing principal.
    WinsignAzure,
    /// `aterm fleet join` given no code: it joins the default ALab fleet with the vault's.
    FleetJoin,
    /// Roster signing: `atpkg-keys join`, `machine-revoke` and `master-check`, and `ship
    /// provision`'s mint.
    ReleaseRoster,
    /// A person or an agent at `aterm secrets need <want>`.
    ByHand,
    /// An entry a stolen Mac could read, filed for a new value by `revoke --stolen` or
    /// `members remove --stolen` ([`super::raise_rotations`]). Never posted: a holder's offer
    /// would be another copy of the value that left; an `aterm secrets set` of the entry on
    /// this Mac in each section the Mac read it in closes it ([`super::rotated`]).
    StolenRotation,
}

impl Purpose {
    /// Every purpose.
    pub const ALL: [Self; 14] = [
        Self::ReleaseSiteKey,
        Self::ReleaseNotary,
        Self::ReleaseDeveloperId,
        Self::HarnessSignIn,
        Self::AghDev,
        Self::AghRelease,
        Self::Commons,
        Self::MachinesKeys,
        Self::MachinesEnroll,
        Self::WinsignAzure,
        Self::FleetJoin,
        Self::ReleaseRoster,
        Self::ByHand,
        Self::StolenRotation,
    ];

    /// Its compiled id.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::ReleaseSiteKey => "release.site-key",
            Self::ReleaseNotary => "release.notary",
            Self::ReleaseDeveloperId => "release.developer-id",
            Self::HarnessSignIn => "harness.sign-in",
            Self::AghDev => "agh.dev",
            Self::AghRelease => "agh.release",
            Self::Commons => "commons.control",
            Self::MachinesKeys => "machines.keys",
            Self::MachinesEnroll => "machines.enroll",
            Self::WinsignAzure => "winsign.azure",
            Self::FleetJoin => "fleet.join",
            Self::ReleaseRoster => "release.roster",
            Self::ByHand => "by-hand",
            Self::StolenRotation => "revoke.stolen",
        }
    }

    /// The purpose `id` names.
    #[must_use]
    pub fn parse(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|purpose| purpose.id() == id)
    }

    /// Whether a consumer in this build runs its own check and reports it
    /// ([`super::satisfied`]): `ship provision`'s deployer-key, notary and Developer ID rows, the
    /// sign-in ladder, and the default repository consumer. Any other need (one raised by hand, or for a consumer not wired yet) has no check of its own,
    /// so the vault version that gives this Mac the want's entry is what fulfils it; without
    /// that it would stay published for good.
    #[must_use]
    pub const fn checks_itself(self) -> bool {
        matches!(
            self,
            Self::ReleaseSiteKey
                | Self::ReleaseNotary
                | Self::ReleaseDeveloperId
                | Self::HarnessSignIn
                | Self::AghDev
        )
    }

    /// Whether its needs are ever posted to the lab: every purpose's but a stolen Mac's
    /// rotations, which want a new value, not a copy of the one that left.
    #[must_use]
    pub const fn asks_the_lab(self) -> bool {
        !matches!(self, Self::StolenRotation)
    }
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
