// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! [`Ctx`]: everything a verb reads from the machine, resolved once and passed down.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::error::{Error, Result};
use crate::host::agh::AghHost;
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
        match self {
            HostChoice::Gh => "gh",
            HostChoice::AghStaging => "agh-staging",
            HostChoice::Agh => "agh",
        }
    }

    /// The choice `text` names.
    #[must_use]
    pub fn parse(text: &str) -> Option<HostChoice> {
        match text {
            "gh" => Some(HostChoice::Gh),
            "agh-staging" => Some(HostChoice::AghStaging),
            "agh" => Some(HostChoice::Agh),
            _ => None,
        }
    }

    /// A verb's own `--host VALUE` or `--host=VALUE`: the value (`(missing)` when `--host`
    /// ends the line, taking the next argument from `rest`), or `None` for any other `flag`.
    pub(crate) fn flag_value<'a>(
        flag: &'a str,
        rest: &mut impl Iterator<Item = &'a String>,
    ) -> Option<&'a str> {
        if flag == "--host" {
            return Some(rest.next().map_or("(missing)", String::as_str));
        }
        flag.strip_prefix("--host=")
    }
}

/// The two config needs `from_home_hosts` raises and clears (design §10.3).
const CONFIG_NEEDS: [&str; 2] = ["agh.config.missing", "agh.config.invalid"];

/// What `AghHost::production`'s refusal begins with when agh.toml does not validate.
const INVALID: &str = "agh.toml is invalid: ";

/// What `AghHost::production`'s refusal begins with when agh.toml is missing.
const MISSING: &str = "agh.toml is missing";

/// What `AghHost::production`'s refusal begins with when agh.toml says `vault = true`.
const VAULT: &str = "agh.toml says vault = true";

/// Why the lanes refuse an agh.toml that says `vault = true`: the fact alone, because naming
/// writers beside the line never clears it (`production` refuses the line whatever profiles
/// the file names), and [`crate::hosts_check::UNVAULT`] is the step that does.
const VAULT_WHY: &str =
    "vault adoption is not composed for atpkg-publish, whatever profiles agh.toml names";

/// The need `from_home_hosts` raises for production's refusal `why`, at `at`. Each `then` is a
/// step that clears that refusal: `agh.config.missing` is agh's own, whose `then` is the step
/// that provisions agh.toml from token files ([`crate::hosts_check::PROVISION`] is the same
/// constant), and a file that says `vault = true` is `agh.config.invalid` whose `why` is
/// [`VAULT_WHY`] and whose `then` is [`crate::hosts_check::UNVAULT`], rather than
/// `agh policy check`, which accepts the file.
fn config_need(why: &str, at: u64) -> agh::needs::Need {
    use crate::hosts_check::UNVAULT;
    if why.starts_with(MISSING) {
        agh::needs::Need::config_missing(at)
    } else if why.starts_with(VAULT) {
        let mut need = agh::needs::Need::config_invalid(VAULT_WHY, at);
        need.what = "agh.toml says vault = true on this builder".to_string();
        need.then = UNVAULT.to_string();
        need
    } else {
        agh::needs::Need::config_invalid(why.strip_prefix(INVALID).unwrap_or(why), at)
    }
}

/// `from_home_hosts`' refusal for production's `why`, with `shown` the config file and `h`
/// the choice. A `vault = true` refusal is spelled from [`VAULT_WHY`] and
/// [`crate::hosts_check::UNVAULT`], the step that clears it, never from `production`'s own
/// advice.
fn config_refusal(why: &str, shown: &str, h: &str) -> String {
    let why = if why.starts_with(VAULT) {
        format!("{VAULT}, and {VAULT_WHY}: {}", crate::hosts_check::UNVAULT)
    } else {
        why.to_string()
    };
    format!("{why}: the lanes route their GitHub identities through it ({shown}, --host {h})")
}

/// Clear both config needs under `dirs` (best effort, as `from_home_hosts`' success does).
/// It reads no agh.toml and creates nothing: a missing needs folder stays missing.
pub(crate) fn clear_config_needs(dirs: &agh::config::Dirs) {
    for key in CONFIG_NEEDS {
        let _ = agh::needs::clear(dirs, key);
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
        Self::from_env_hosts(HostChoice::Gh)
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
    /// missing or invalid (design §10.3's text), after raising the need (§4.6.15). Also
    /// could-not-run when agh's directories cannot be resolved from `HOME`, `XDG_CONFIG_HOME`
    /// and `XDG_STATE_HOME` (a relative root), when an owner of `accounts` is no valid slug
    /// owner, and when agh.toml says `vault = true` (its need is `agh.config.invalid`).
    pub fn from_env_hosts(hosts: HostChoice) -> Result<Self> {
        let home = non_empty_env("HOME").ok_or_else(|| {
            Error::cannot_run(
                "HOME is unset — the machine key, the token files, the state root and the \
                 atpkg store are all found under it; run with HOME set",
            )
        })?;
        let now = aterm_time::unix_secs()
            .and_then(|s| i64::try_from(s).ok())
            .ok_or_else(|| Error::cannot_run("the system clock reads before 1970"))?;
        Self::from_home_hosts(
            &home,
            non_empty_env("XDG_CONFIG_HOME").as_deref(),
            non_empty_env("XDG_STATE_HOME").as_deref(),
            now,
            hosts,
        )
    }

    /// What `from_env_hosts` builds once it has read the environment and the clock: the
    /// context of `home`, its XDG directories and `now_unix`, with `hosts`' hosts. Tests call
    /// it over a scratch home; `from_env_hosts` reads `HOME`, `XDG_CONFIG_HOME`,
    /// `XDG_STATE_HOME` and the clock as `from_env` does, then calls it.
    ///
    /// # Errors
    ///
    /// As `from_env_hosts`. Its unset-`HOME` and clock refusals are its own: this reads
    /// neither.
    pub fn from_home_hosts(
        home: &Path,
        xdg_config_home: Option<&Path>,
        xdg_state_home: Option<&Path>,
        now_unix: i64,
        hosts: HostChoice,
    ) -> Result<Self> {
        let tokens = Tokens::under(home);
        let gh = Hosts {
            staging: Arc::new(RestHost::new(Some(tokens.staging))),
            channel: Arc::new(RestHost::new(Some(tokens.channel))),
        };
        let mut ctx = Self::new(home, xdg_config_home, xdg_state_home, now_unix, gh);
        ctx.host_choice = hosts;
        if hosts == HostChoice::Gh {
            // The default reads no agh.toml (plan §1, rule 16).
            return Ok(ctx);
        }
        let h = hosts.as_str();
        let dirs =
            agh::config::Dirs::resolve(home, xdg_config_home, xdg_state_home).ok_or_else(|| {
                let shown = |dir: Option<&Path>| {
                    dir.map_or_else(|| "(unset)".to_string(), |d| d.display().to_string())
                };
                Error::cannot_run(format!(
                    "agh's directories cannot be resolved from HOME {}, XDG_CONFIG_HOME {} and \
                     XDG_STATE_HOME {} (each must be absolute): --host {h} reads agh.toml there",
                    home.display(),
                    shown(xdg_config_home),
                    shown(xdg_state_home)
                ))
            })?;
        let slug = |owner: &str| {
            agh::Slug::new(owner, atpkg::manifest::INDEX_REPO)
                .map_err(|e| Error::cannot_run(e.why.clone()))
        };
        let (staging_home, public_home) =
            (slug(&ctx.accounts.staging)?, slug(&ctx.accounts.public)?);
        let built = AghHost::production(&dirs, staging_home).and_then(|staging| {
            let channel: crate::host::HostRef = match hosts {
                HostChoice::Agh => Arc::new(AghHost::production(&dirs, public_home)?),
                _ => ctx.hosts.channel.clone(),
            };
            Ok((staging, channel))
        });
        let (staging, channel) = match built {
            Ok(built) => built,
            Err(why) => {
                // Nothing was sent: `production` returns before any client is built. The need
                // is a fact for later; the refusal below names the file whatever it answers.
                let need = config_need(&why, u64::try_from(now_unix).unwrap_or(0));
                let _ = agh::needs::raise(&dirs, &need);
                for key in CONFIG_NEEDS.iter().filter(|k| **k != need.key) {
                    let _ = agh::needs::clear(&dirs, key);
                }
                return Err(Error::cannot_run(config_refusal(
                    &why,
                    &ctx.shown(&dirs.config_file()),
                    h,
                )));
            }
        };
        clear_config_needs(&dirs);
        ctx.hosts = Hosts {
            staging: Arc::new(staging),
            channel,
        };
        Ok(ctx)
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
