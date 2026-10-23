// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! [`Ctx`]: everything a verb reads from the machine, resolved once and passed down.
//!
//! Environment knobs are read only in [`Ctx::from_env`] (an unset `HOME` is a named
//! could-not-run); everything else a verb could vary is a typed flag on that verb. Tests
//! build a `Ctx` with [`Ctx::new`] over scratch directories, an injected anchor and clock,
//! and `FakeHost` hosts (`crate::host::fake`).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::error::{Error, Result};
use crate::host::rest::RestHost;
use crate::host::{Hosts, Slug};

/// The private development owner packs are uploaded to before they are made public (the
/// export rewrites this spelling to the public owner, as it does everywhere else).
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
    /// The secret pkcs8 machine key (`~/.aterm/machine.key`), minted by
    /// `atpkg-keys setup`/`join`.
    pub key: PathBuf,
    /// Its public record (`~/.aterm/machine.toml`): the roster id and pubkey.
    pub record: PathBuf,
    /// The machine's installed roster pair (`~/.aterm/roster/aterm-machines.toml`), one
    /// of the candidates the newest verified roster is chosen from.
    pub roster: PathBuf,
}

/// The resolved machine.
#[derive(Clone)]
pub struct Ctx {
    /// `$HOME`.
    pub home: PathBuf,
    /// The aterm checkout this binary was built from (for `build-local` and `lane
    /// install`); the snapshot lanes never read it.
    pub repo_root: PathBuf,
    /// The lanes' state root (`$XDG_STATE_HOME/aterm-auto`, else
    /// `~/.local/state/aterm-auto`).
    pub state_dir: PathBuf,
    /// The atpkg prefix (`atpkg::toolchain::atpkg_prefix`): where the store's Trust
    /// toolchain is resolved from.
    pub pkg_prefix: PathBuf,
    /// The trust checkout the lineage receipt switch is read from (`$HOME/trust`; a verb's
    /// `--trust-repo` names another). One path for every reader, so a group's preflight
    /// and the packs it drives read the same switch.
    pub trust_repo: PathBuf,
    /// The accounts.
    pub accounts: Accounts,
    /// The token files.
    pub tokens: Tokens,
    /// This machine's identity files.
    pub identity: IdentityPaths,
    /// The paper-master keys every roster is verified under: the compiled
    /// `pins::PAPER_MASTER_PUBKEYS` (tests inject their own).
    pub anchor: Vec<String>,
    /// The clock, read once.
    pub now_unix: i64,
    /// The release hosts.
    pub hosts: Hosts,
    /// The roster generation this context's packs sign under, elected once
    /// ([`crate::artifact::sign::elected`]) and shared by every clone: a lane or a group
    /// reads the channel head for it once, not once per pack.
    pub(crate) elected: Arc<Mutex<Option<Arc<crate::artifact::sign::Authority>>>>,
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
            staging: Arc::new(RestHost::new(token_if_present(&tokens.staging))),
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

    /// A context rooted at `home` that speaks to `hosts`, with the compiled anchor and
    /// production defaults for every path.
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
                key: home.join(".aterm/machine.key"),
                record: home.join(".aterm/machine.toml"),
                roster: home.join(".aterm/roster/aterm-machines.toml"),
            },
            anchor: aterm_update_core::pins::PAPER_MASTER_PUBKEYS
                .iter()
                .map(|k| (*k).to_string())
                .collect(),
            now_unix,
            hosts,
            elected: Arc::default(),
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

    /// `path` with a leading `$HOME` shown as `~`, for anything printed: transcripts are
    /// pasted beside public releases, and an absolute home path names the operator's
    /// account.
    #[must_use]
    pub fn shown(&self, path: &Path) -> String {
        match path.strip_prefix(&self.home) {
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => path.display().to_string(),
        }
    }
}

fn non_empty_env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn token_if_present(path: &Path) -> Option<PathBuf> {
    std::fs::metadata(path)
        .ok()
        .filter(|m| m.len() > 0)
        .map(|_| path.to_path_buf())
}
