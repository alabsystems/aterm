// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WANT CATALOG (docs/DESIGN-credential-requests-2026-10-27.md §4), compiled in: what a Mac
//! may ask the lab for, where copies of each live by design, who can make a new one, the
//! entry it arrives as, the hold probe a holder runs (metadata only), and why a want that is
//! never asked is never asked. No record and no admin can make a want askable that this file
//! does not.

use std::path::{Path, PathBuf};

/// The one path `publish/site.toml`'s `firebase_sa_key` resolves to, relative to its `dir`
/// (`crates/aterm-release/src/site/mod.rs`): the vault entry the site deployer's key arrives as.
pub const SITE_DEPLOYER_KEY_ENTRY: &str =
    "~/company-life/config/local/service-account.alab-site-deployer.json";

/// The notary credential consumed by the release subscriber. The legacy `notary` entry
/// triggers older clients before their developer-tools checks, so it is never supplied.
pub const NOTARY_ENTRY: &str = "release-notary";

/// The catalog's version, cited as a catalog retirement's evidence.
pub const VERSION: u32 = 1;

/// The most characters a mailbox label or a `login:` name takes.
const MAX_LABEL: usize = 63;

/// Which GitHub token a `gh:` want names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GhProfile {
    /// `gh_access_token`, agh's dev identity.
    Dev,
    /// `gh_access_token_alabsystems`, the release identity.
    Release,
}

/// What a request names: never a file path a record could choose.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Want {
    /// `site-deployer-key`: the site deployer's service-account key.
    SiteDeployerKey,
    /// `notary`: the notary credential (`~/.secrets/release-notary`).
    Notary,
    /// `mailbox:<label>`: one AI account's mailbox password (and TOTP seed).
    Mailbox(String),
    /// `gh:<dev|release>`: a vault GitHub token.
    Gh(GhProfile),
    /// `entry:<name>`: any vault entry by name, answered from this Mac's own envelope.
    Entry(String),
    /// `paper-master`: the release roster's paper master.
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
}

/// Where a want's copies live by design, which decides whether it is ever posted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Copies live on Macs' disks (release Macs): posted, and holders may offer one.
    Copyable,
    /// Copies live in the vault by design: posted; a stray copy may still be offered.
    VaultOnly,
    /// It exists on no computer.
    Nowhere,
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
            Self::Nowhere => "nowhere",
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
                "{word:?}: {what} (wants: site-deployer-key, notary, mailbox:<label>, gh:dev, \
                 gh:release, entry:<name>, paper-master, login:<provider>/<label>, \
                 browser-profile:<label>, refresh-token:<provider>/<label>, vault-identity, \
                 commons-device-key, commons-control)"
            ))
        };
        let want = match word {
            "site-deployer-key" => Self::SiteDeployerKey,
            "notary" => Self::Notary,
            "paper-master" => Self::PaperMaster,
            "vault-identity" => Self::VaultIdentity,
            "commons-device-key" => Self::CommonsDeviceKey,
            "commons-control" => Self::CommonsControl,
            "gh:dev" => Self::Gh(GhProfile::Dev),
            "gh:release" => Self::Gh(GhProfile::Release),
            _ => {
                let Some((kind, rest)) = word.split_once(':') else {
                    return bad("not a want the catalog names");
                };
                match kind {
                    "mailbox" if valid_label(rest) => Self::Mailbox(rest.to_owned()),
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
                    "mailbox" | "login" | "refresh-token" | "browser-profile" => {
                        return bad("not a valid label");
                    }
                    "entry" => return bad("not a valid vault entry name"),
                    "gh" => return bad("gh: takes dev or release"),
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
            Self::Mailbox(label) => format!("mailbox:{label}"),
            Self::Gh(GhProfile::Dev) => "gh:dev".to_owned(),
            Self::Gh(GhProfile::Release) => "gh:release".to_owned(),
            Self::Entry(name) => format!("entry:{name}"),
            Self::PaperMaster => "paper-master".to_owned(),
            Self::Login(name) => format!("login:{name}"),
            Self::BrowserProfile(label) => format!("browser-profile:{label}"),
            Self::RefreshToken(name) => format!("refresh-token:{name}"),
            Self::VaultIdentity => "vault-identity".to_owned(),
            Self::CommonsDeviceKey => "commons-device-key".to_owned(),
            Self::CommonsControl => "commons-control".to_owned(),
        }
    }

    /// Where its copies live by design.
    #[must_use]
    pub const fn class(&self) -> Class {
        match self {
            Self::SiteDeployerKey | Self::Notary => Class::Copyable,
            Self::Mailbox(_) | Self::Gh(_) | Self::Entry(_) => Class::VaultOnly,
            Self::PaperMaster => Class::Nowhere,
            Self::Login(_)
            | Self::BrowserProfile(_)
            | Self::RefreshToken(_)
            | Self::VaultIdentity
            | Self::CommonsDeviceKey => Class::PerMac,
            Self::CommonsControl => Class::AdminAct,
        }
    }

    /// Whether a request for it is ever posted to the lab: copyable wants and the vault-only
    /// `mailbox:*` and `gh:*`. `entry:<name>` is answered from this Mac's own envelope.
    #[must_use]
    pub const fn posted(&self) -> bool {
        matches!(
            self,
            Self::SiteDeployerKey | Self::Notary | Self::Mailbox(_) | Self::Gh(_)
        )
    }

    /// Whether a person can make a new one when no Mac holds it.
    #[must_use]
    pub const fn person_sourced(&self) -> bool {
        matches!(
            self,
            Self::SiteDeployerKey | Self::Notary | Self::Mailbox(_) | Self::Gh(_)
        )
    }

    /// The vault entries it arrives as, the first the one a request is answered by.
    #[must_use]
    pub fn entries(&self) -> Vec<String> {
        match self {
            Self::SiteDeployerKey => vec![SITE_DEPLOYER_KEY_ENTRY.to_owned()],
            Self::Notary => vec![NOTARY_ENTRY.to_owned()],
            Self::Mailbox(label) => vec![format!("{label}_password"), format!("{label}_totp")],
            Self::Gh(GhProfile::Dev) => vec!["gh_access_token".to_owned()],
            Self::Gh(GhProfile::Release) => vec!["gh_access_token_alabsystems".to_owned()],
            Self::Entry(name) => vec![name.clone()],
            Self::CommonsControl => vec![crate::commons::ENTRY.to_owned()],
            Self::PaperMaster
            | Self::Login(_)
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
        matches!(self, Self::SiteDeployerKey | Self::Notary)
    }

    /// What it is, in a sentence's words.
    #[must_use]
    pub fn title(&self) -> String {
        match self {
            Self::SiteDeployerKey => "the site deployer's service-account key".to_owned(),
            Self::Notary => "the notary credential".to_owned(),
            Self::Mailbox(label) => format!("{label}'s mailbox password"),
            Self::Gh(GhProfile::Dev) => "the dev GitHub token".to_owned(),
            Self::Gh(GhProfile::Release) => "the release GitHub token".to_owned(),
            Self::Entry(name) => format!("the vault entry {name}"),
            Self::PaperMaster => "the paper master".to_owned(),
            Self::Login(name) => format!("the vendor login {name}"),
            Self::BrowserProfile(label) => format!("{label}'s browser profile"),
            Self::RefreshToken(name) => format!("the refresh token of {name}"),
            Self::VaultIdentity => "this Mac's vault identity".to_owned(),
            Self::CommonsDeviceKey => "this Mac's Commons device key".to_owned(),
            Self::CommonsControl => "the Commons control entry".to_owned(),
        }
    }

    /// Who makes a new one when no Mac holds it.
    #[must_use]
    pub fn source(&self) -> &'static str {
        match self {
            Self::SiteDeployerKey => {
                "a person with owner rights on the site's Google Cloud project mints a key for the alab-site-deployer service account"
            }
            Self::Notary => "a person makes an app-specific password at account.apple.com",
            Self::Mailbox(_) => "the mailbox's own password, which only its owner holds",
            Self::Gh(_) => "a person makes a new token at github.com/settings/tokens",
            Self::Entry(_) => "the vault's admin",
            Self::PaperMaster => "paper",
            Self::Login(_) | Self::BrowserProfile(_) | Self::RefreshToken(_) => {
                "the vendor, on each Mac"
            }
            Self::VaultIdentity | Self::CommonsDeviceKey => "this Mac",
            Self::CommonsControl => "the vault admin's aterm",
        }
    }

    /// The retirement the catalog answers at once, before anything leaves this Mac: its
    /// reason and the explanation `need ls` prints. `None` for a want that may be posted, for
    /// `entry:<name>` (answered from the envelope) and for the admin act (a local wait).
    #[must_use]
    pub fn catalog_retirement(&self) -> Option<(super::Reason, String)> {
        use super::Reason;
        match self.class() {
            Class::Nowhere => Some((
                Reason::NeverStored,
                "the paper master is 52 base32 characters on paper and exists on no computer \
                 (docs/RELEASE-KEYS.md:14); machines hold only PAPER_MASTER_PUBKEYS \
                 (crates/aterm-update-core/src/pins.rs:8), and roster signing asks for it at \
                 the terminal that signs"
                    .to_owned(),
            )),
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

    /// The hold probe's one path under `home`: a holder offers only what this resolves to, and
    /// the probe stats it, never reads it. `None` for a want no holder can offer.
    #[must_use]
    pub fn hold_path(&self, home: &Path) -> Option<PathBuf> {
        match self {
            Self::SiteDeployerKey => {
                crate::materialize::target_path(home, SITE_DEPLOYER_KEY_ENTRY).ok()
            }
            Self::Notary => Some(home.join(".secrets").join(NOTARY_ENTRY)),
            Self::Mailbox(label) => Some(home.join(".secrets").join(format!("{label}_password"))),
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
    /// The Claude Code sign-in ladder's mailbox route.
    HarnessSignIn,
    /// agh's dev identity.
    AghDev,
    /// agh's release identity.
    AghRelease,
    /// Commons readiness.
    Commons,
    /// A person or an agent at `aterm secrets need <want>`.
    ByHand,
}

impl Purpose {
    /// Every purpose.
    pub const ALL: [Self; 7] = [
        Self::ReleaseSiteKey,
        Self::ReleaseNotary,
        Self::HarnessSignIn,
        Self::AghDev,
        Self::AghRelease,
        Self::Commons,
        Self::ByHand,
    ];

    /// Its compiled id.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::ReleaseSiteKey => "release.site-key",
            Self::ReleaseNotary => "release.notary",
            Self::HarnessSignIn => "harness.sign-in",
            Self::AghDev => "agh.dev",
            Self::AghRelease => "agh.release",
            Self::Commons => "commons.control",
            Self::ByHand => "by-hand",
        }
    }

    /// The purpose `id` names.
    #[must_use]
    pub fn parse(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|purpose| purpose.id() == id)
    }

    /// Whether a consumer in this build runs its own check and reports it
    /// ([`super::satisfied`]): `ship provision`'s deployer-key and notary rows, and the sign-in ladder. Any
    /// other need (one raised by hand, or for a consumer not wired yet) has no check of its own,
    /// so the vault version that gives this Mac the want's entry is what fulfils it; without
    /// that it would stay published for good.
    #[must_use]
    pub const fn checks_itself(self) -> bool {
        matches!(
            self,
            Self::ReleaseSiteKey | Self::ReleaseNotary | Self::HarnessSignIn
        )
    }
}
