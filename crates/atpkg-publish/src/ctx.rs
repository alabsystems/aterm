// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! [`Ctx`]: everything a verb reads from the machine, resolved once and passed down.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::error::{Error, Result};
use crate::host::rest::RestHost;
use crate::host::{Hosts, Slug};

/// The private development owner packs are staged to before publication.
pub const STAGING_OWNER: &str = "alabsystems";

/// The accounts a publish speaks to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Accounts {
    /// Where packs are staged.
    pub staging: String,
    /// The public channel owner: the index, and every pack it pins.
    pub public: String,
}

/// The token files the two identities read (never an ambient credential in a lane).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tokens {
    /// Write on the staging owner (`~/.secrets/gh_access_token`).
    pub staging: PathBuf,
    /// Write on the public channel owner (`~/.secrets/gh_access_token_alabsystems`).
    pub channel: PathBuf,
}

impl Tokens {
    /// The token files under `home`.
    fn under(home: &Path) -> Self {
        Self {
            staging: home.join(".secrets/gh_access_token"),
            channel: home.join(".secrets/gh_access_token_alabsystems"),
        }
    }
}

/// This machine's publishing identity, as files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentityPaths {
    /// The secret PKCS#8 machine key (`~/.aterm/release-ring/machine.key`).
    pub key: PathBuf,
    /// Its public record (`~/.aterm/release-ring/machine.toml`): the roster id and pubkey.
    pub record: PathBuf,
    /// The machine's installed roster pair (`~/.aterm/release-ring/roster/aterm-machines.toml`).
    pub roster: PathBuf,
}

/// The resolved machine.
#[derive(Clone)]
pub struct Ctx {
    /// `$HOME`.
    pub home: PathBuf,
    /// The aterm checkout this binary was built from (for `build-local` and `lane install`).
    pub repo_root: PathBuf,
    /// The lanes' state root (`$XDG_STATE_HOME/aterm-auto`, else `~/.local/state/aterm-auto`).
    pub state_dir: PathBuf,
    /// The atpkg prefix (`atpkg::toolchain::atpkg_prefix`) for the store's Trust toolchain.
    pub pkg_prefix: PathBuf,
    /// The trust checkout the lineage receipt switch is read from (`$HOME/trust`).
    pub trust_repo: PathBuf,
    /// The accounts.
    pub accounts: Accounts,
    /// The token files.
    pub tokens: Tokens,
    /// This machine's identity files.
    pub identity: IdentityPaths,
    /// The paper-master keys every roster is verified under (`pins::PAPER_MASTER_PUBKEYS`).
    pub anchor: Vec<String>,
    /// The clock, read once.
    pub now_unix: i64,
    /// The release hosts.
    pub hosts: Hosts,
    /// The roster generation this context's packs sign under, elected once and shared across clones.
    pub(crate) elected: Arc<Mutex<Option<Arc<crate::artifact::sign::Authority>>>>,
    /// The choice this context's hosts were built with (§16.1). `pub(crate)`, not private,
    /// because `channel/group.rs` builds `Ctx { trust_repo, ..ctx.clone() }`.
    pub(crate) host_choice: HostChoice,
}

/// Which hosts a run builds (§16.1): `gh` is today's, and the default until WP15.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HostChoice {
    /// `RestHost` for both hosts.
    #[default]
    Gh,
    /// `AghHost` for staging, `RestHost` for the channel: the staging passes of §16.1.
    AghStaging,
    /// `AghHost` for both hosts.
    Agh,
}

impl HostChoice {
    /// `gh`, `agh-staging`, `agh`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        // WP12 writes the spellings (plan §16); until then the choice has none.
        let _ = self;
        ""
    }

    /// The choice `text` names.
    #[must_use]
    pub fn parse(text: &str) -> Option<HostChoice> {
        // WP12 writes the parse (plan §16); until then no text names a choice.
        let _ = text;
        None
    }
}

impl Ctx {
    /// The machine this process runs on.
    ///
    /// # Errors
    ///
    /// A could-not-run naming `HOME` when it is unset or empty, or when the clock cannot
    /// be read.
    pub fn from_env() -> Result<Self> {
        let home = non_empty_env("HOME").ok_or_else(|| {
            Error::cannot_run(
                "HOME is unset — the machine key, the token files, the state root and the \
                 atpkg store are all found under it; run with HOME set",
            )
        })?;
        let now = aterm_time::unix_secs()
            .and_then(|s| i64::try_from(s).ok())
            .ok_or_else(|| Error::cannot_run("the system clock reads before 1970"))?;
        let tokens = Tokens::under(&home);
        let hosts = Hosts {
            staging: Arc::new(RestHost::new(Some(tokens.staging))),
            channel: Arc::new(RestHost::new(Some(tokens.channel))),
        };
        Ok(Self::new(
            &home,
            non_empty_env("XDG_CONFIG_HOME").as_deref(),
            non_empty_env("XDG_STATE_HOME").as_deref(),
            now,
            hosts,
        ))
    }

    /// A context rooted at `home` that speaks to `hosts` with production defaults.
    #[must_use]
    pub fn new(
        home: &Path,
        xdg_config_home: Option<&Path>,
        xdg_state_home: Option<&Path>,
        now_unix: i64,
        hosts: Hosts,
    ) -> Self {
        let state_root =
            xdg_state_home.map_or_else(|| home.join(".local/state"), Path::to_path_buf);
        Self {
            home: home.to_path_buf(),
            repo_root: Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."),
            state_dir: state_root.join("aterm-auto"),
            pkg_prefix: atpkg::toolchain::atpkg_prefix(home, xdg_config_home),
            trust_repo: home.join("trust"),
            accounts: Accounts {
                staging: STAGING_OWNER.to_string(),
                public: aterm_update_core::ATPKG_INDEX_OWNER.to_string(),
            },
            tokens: Tokens::under(home),
            identity: IdentityPaths {
                key: home.join(atpkg_keys::provision::MACHINE_KEY_REL),
                record: home.join(atpkg_keys::provision::MACHINE_PUB_REL),
                roster: home.join(atpkg_keys::provision::RELEASE_ROSTER_REL),
            },
            anchor: aterm_update_core::pins::PAPER_MASTER_PUBKEYS
                .iter()
                .map(|k| (*k).to_string())
                .collect(),
            now_unix,
            hosts,
            elected: Arc::default(),
            host_choice: HostChoice::Gh,
        }
    }

    /// [`Ctx::from_env`] with `hosts` (§16.1). `from_env()` is `from_env_hosts(HostChoice::Gh)`.
    ///
    /// # Errors
    ///
    /// As `from_env`; under `AghStaging` or `Agh`, a could-not-run naming agh.toml when it is
    /// missing or invalid (design §10.3's text), after raising the need (§4.6.15).
    pub fn from_env_hosts(hosts: HostChoice) -> Result<Self> {
        let _ = hosts;
        Err(Error::cannot_run(
            "not yet: Ctx::from_env_hosts lands in WP12",
        ))
    }

    /// What `from_env_hosts` builds once it has read the environment and the clock: the
    /// context of `home`, its XDG directories and `now_unix`, with `hosts`' hosts. Tests call
    /// it over a scratch home; `from_env_hosts` reads `HOME`, `XDG_CONFIG_HOME`,
    /// `XDG_STATE_HOME` and the clock as `from_env` does, then calls it.
    ///
    /// # Errors
    ///
    /// As `from_env_hosts`.
    pub fn from_home_hosts(
        home: &Path,
        xdg_config_home: Option<&Path>,
        xdg_state_home: Option<&Path>,
        now_unix: i64,
        hosts: HostChoice,
    ) -> Result<Self> {
        let _ = (home, xdg_config_home, xdg_state_home, now_unix, hosts);
        Err(Error::cannot_run(
            "not yet: Ctx::from_home_hosts lands in WP12",
        ))
    }

    /// The choice this context's hosts were built with.
    #[must_use]
    pub fn host_choice(&self) -> HostChoice {
        self.host_choice
    }

    /// Tests: the same context, marked as built for `choice`. The hosts stay the test's own (an
    /// `AghHost` over agh-fake, a `FakeHost`); only the pre-claim checks read the mark.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn with_host_choice(self, choice: HostChoice) -> Ctx {
        Ctx {
            host_choice: choice,
            ..self
        }
    }

    /// The channel repo the index is published on (`<public>/aterm`).
    #[must_use]
    pub fn index_slug(&self) -> Slug {
        Slug::new(&self.accounts.public, atpkg::manifest::INDEX_REPO)
    }

    /// A program repo under the staging owner.
    #[must_use]
    pub fn staging_slug(&self, repo: &str) -> Slug {
        Slug::new(&self.accounts.staging, repo)
    }

    /// A program repo under the public owner.
    #[must_use]
    pub fn public_slug(&self, repo: &str) -> Slug {
        Slug::new(&self.accounts.public, repo)
    }

    /// `path` with a leading `$HOME` replaced by `~`.
    #[must_use]
    pub fn shown(&self, path: &Path) -> String {
        match path.strip_prefix(&self.home) {
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => path.display().to_string(),
        }
    }

    /// Whether `path` holds a usable GitHub token (`Ok(false)` when absent or whitespace-only).
    ///
    /// # Errors
    ///
    /// Returns the reason (with `path` redacted through [`Self::shown`]) when `path` cannot be
    /// read or holds a byte outside visible ASCII.
    pub(crate) fn usable_token(&self, path: &Path) -> std::result::Result<bool, String> {
        aterm_update_core::github_rest::Token::from_file(path)
            .map(|token| token.is_some())
            .map_err(|why| why.replace(&path.display().to_string(), &self.shown(path)))
    }
}

fn non_empty_env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}
