// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! atpkg's reader for its tables of the same `aterm.toml` the GUI reads (§11): `[packages]`,
//! `[machine]` and `[reroute]`. Settings, never env vars (`ATPKG_REGISTRY` is a development
//! seam, `aterm_types::dev_seam!`).
//!
//! `[packages]` keys a person has (Settings ▸ Packages writes the first two):
//!
//! * `enabled`: Automatic updates, default true. Off, no automatic lane runs; a typed verb
//!   still works. A retired `auto_update = false` still reads as off
//!   ([`PackagesConfig::enabled`]).
//! * `auto_install`: the install consent, default true (installing aterm is wanting the
//!   toolset). [`PackagesConfig::auto_install`] has the rule and the `seed_install`
//!   migration.
//! * `exclude`: per-program opt-out, narrowing-only over the signed index
//!   ([`crate::manifest::Index::installable`]).
//!
//! Three development settings a shipped binary drops at load with a notice
//! ([`DEV_SEAMS`]): `prefix`, `account` and the `owner/repo` form of `[packages.links]`
//! (a checkout-path link stays). `channel` and `include` are retired: named once, ignored.
//! Nothing here is a trust input: a link can only redirect where bytes are fetched from or
//! suppress registry management, never what verifies.
//!
//! A missing file or table yields the defaults. A `[packages]` table that cannot be read
//! falls to [`PackagesConfig::unreadable_table`], loudly: `auto_install` defaults true and
//! `exclude` empty, and a typo anywhere in the file must not reset the keys that prevent
//! multi-GB downloads. Nothing new installs unattended; what is installed keeps updating,
//! and a typed `install` still installs.
//!
//! `[machine]` defaults act on the host (a `com.apple.universalcontrol` write), so an
//! unreadable file falls to [`MachineConfig::inert`] instead. Each table is deserialized
//! on its own ([`PackagesOnly`], [`MachineOnly`]), so a typo costs only its own table.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Maximum `aterm.toml` size read, matching the native config service's 512 KiB budget.
pub const MAX_PACKAGES_CONFIG_BYTES: usize = 512 * 1024;

/// The one channel whose pin set drives install/update. `[packages].channel` is retired:
/// only `stable` is published.
pub const CHANNEL: &str = "stable";

/// Whether this build honours the development settings of `[packages]` (`prefix`,
/// `account`, the `owner/repo` form of `[packages.links]`): only a development build does
/// (`debug_assertions`, or the `dev-seams` feature the release cutter never enables). A
/// shipped binary drops them at load and says so once ([`admit_packages`]).
pub const DEV_SEAMS: bool = cfg!(any(debug_assertions, feature = "dev-seams"));

/// The `[packages]` table. All-Option (an absent key and a default-valued key are
/// indistinguishable); defaults live only in the resolver methods below.
#[derive(Debug, Default, Clone, serde::Deserialize)]
#[serde(default)]
pub struct PackagesConfig {
    /// `[packages].prefix`: the install prefix override; absent ⇒
    /// [`crate::store::default_prefix`]. [`crate::store::vet_prefix`] admits only a `$HOME`
    /// chain owned by our uid or a chain from `/` owned by root, with no group/other-writable
    /// or symlinked component, and falls back to the default otherwise.
    ///
    /// The verified lane does not need it (Trust's default `CallerOwned` authority proves the
    /// `$HOME` prefix with no privilege); its uses are a system-wide store shared by several
    /// users, or `TRUST_REQUIRE_SEALED_LAUNCHER=1` (`docs/GOLDEN-INSTALL-PATH.md` §2). A
    /// development setting ([`DEV_SEAMS`]): a shipped binary drops it at load and, while it
    /// names another store, installs nothing unattended ([`Self::ignored_prefix`]).
    pub prefix: Option<String>,
    /// `[packages].enabled`: Automatic updates (Settings ▸ Packages), read through
    /// [`Self::enabled`]. Gates the automatic lanes (the window's loop, a terminal
    /// session's detached pass, the vendor head watch) and the unattended set completion,
    /// never a typed verb.
    pub enabled: Option<bool>,
    /// Retired, folded into `enabled`: `auto_update = false` is still honoured
    /// ([`Self::enabled`]) and doctor names the rename; Settings drops this key when it
    /// writes `enabled`.
    pub auto_update: Option<bool>,
    /// `[packages].auto_install`: the install consent (Settings ▸ Packages, "Install the
    /// ALab toolset"), read through [`Self::auto_install`].
    pub auto_install: Option<bool>,
    /// Retired, folded into `auto_install`: `seed_install = false` is still honoured when
    /// `auto_install` is unset ([`Self::auto_install`]), and doctor names the rename.
    pub seed_install: Option<bool>,
    /// Index owner override (e.g. `"alabsystems"`); default the compiled owner
    /// ([`crate::discovery::resolve_account`]). A development setting ([`DEV_SEAMS`]).
    /// Slug-validated downstream, so a malformed value can never redirect fetches.
    pub account: Option<String>,
    /// Retired ([`CHANNEL`]): parsed as any value so a stale key never fails the table,
    /// named once at load, never read.
    pub channel: Option<aterm_toml::Value>,
    /// Retired: the narrowing include filter (a machine keeps the whole signed set minus
    /// `exclude`). Parsed as any value, named once at load, never read.
    pub include: Option<aterm_toml::Value>,
    /// Narrowing-only exclude filter: the per-program opt-out.
    pub exclude: Option<Vec<String>>,
    /// `[packages.links]`: `name = "/path/to/checkout"` (or `~/…`) declares a managed dev
    /// link ([`crate::linkmode`]: registry management skipped; never in Settings);
    /// `name = "owner/repo"` declares a private-repo fetch override for that program's
    /// release assets (verification unchanged), a development setting ([`DEV_SEAMS`]).
    /// Anything else is refused loudly ([`LinkTarget::Invalid`]).
    pub links: BTreeMap<String, String>,
    /// Not a config key (`#[serde(skip)]`, so no `aterm.toml` can set it): true only of the
    /// table [`parse_packages`] could not read ([`PackagesConfig::unreadable_table`]). Every
    /// resolver whose default widens consults it.
    #[serde(skip)]
    pub unreadable: bool,
    /// Not a config key (`#[serde(skip)]`): the `[packages] prefix` a shipped binary
    /// dropped at load ([`admit_packages`]) because it names a store other than the
    /// default, resolved against the home it was read under. While set, nothing installs
    /// unattended ([`Self::installs_unattended`]); removing the key is the whole fix.
    #[serde(skip)]
    pub ignored_prefix: Option<PathBuf>,
    /// Not a config key (`#[serde(skip)]`): the development settings [`admit_packages`]
    /// dropped, in its own words. Said once at load and repeated by doctor
    /// ([`Self::config_notes`]).
    #[serde(skip)]
    pub admission_notes: Vec<String>,
}

impl PackagesConfig {
    /// The reading of a `[packages]` table that failed to parse (the twin of
    /// [`MachineConfig::inert`]): no key is invented; the flag makes the widening
    /// resolvers fail closed.
    #[must_use]
    pub fn unreadable_table() -> Self {
        Self {
            unreadable: true,
            ..Self::default()
        }
    }
    /// Automatic updates: `[packages].enabled`, default true. A retired `auto_update =
    /// false` still reads as off, whatever `enabled` says, until the file is rewritten. A
    /// table we could not read is on: what is installed keeps updating.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(true) && self.auto_update != Some(false)
    }

    /// The `[packages].account` override for [`crate::discovery::resolve_account`]
    /// (`None` ⇒ compiled default; a shipped binary never has one — [`DEV_SEAMS`]).
    #[must_use]
    pub fn account(&self) -> Option<&str> {
        self.account
            .as_deref()
            .map(str::trim)
            .filter(|a| !a.is_empty())
    }

    /// The install consent: whether atpkg may install toolset members nobody named (the
    /// first-run `seed`, and what the signed default set gains). Default true (§9.1): the
    /// set is a multi-GB download and this key is the one switch that prevents it.
    ///
    /// The explicit opt-outs always win (`uninstall <p>`'s removed marker, `uninstall
    /// --all`'s decline, `exclude`, `enabled = false`); a typed verb (`install <p>`,
    /// `install --default-set`) is its own consent. `auto_install` wins when written, else
    /// the retired `seed_install`, else true. A table we could not read resolves false, so a
    /// typo elsewhere in `aterm.toml` never repeals an `auto_install = false`.
    #[must_use]
    pub fn auto_install(&self) -> bool {
        !self.unreadable && self.auto_install.or(self.seed_install).unwrap_or(true)
    }

    /// Whether atpkg may install anything nobody named, here and now: the install consent
    /// ([`Self::auto_install`]) and no ignored prefix ([`Self::ignored_prefix`]), read by
    /// the first-run seed and the pass's set completion. A shipped binary keeps one store,
    /// so a `[packages] prefix` naming another never gets a second store filled beside it.
    #[must_use]
    pub fn installs_unattended(&self) -> bool {
        self.auto_install() && self.ignored_prefix.is_none()
    }

    /// What doctor says about this table's spelling, one line per key to rename or remove:
    /// the retired `auto_update`/`seed_install` (still honoured) and `channel`/`include`
    /// (ignored), plus what [`admit_packages`] dropped.
    #[must_use]
    pub fn config_notes(&self) -> Vec<String> {
        let mut notes = Vec::new();
        if let Some(value) = self.auto_update {
            notes.push(auto_update_note(value, self.enabled));
        }
        if let Some(value) = self.seed_install {
            let honoured = self.auto_install.is_none();
            notes.push(if honoured {
                format!(
                    "[packages] seed_install = {value} is retired — it is read as \
                     `auto_install = {value}`; rename it to `auto_install`"
                )
            } else {
                format!(
                    "[packages] seed_install = {value} is retired and `auto_install` \
                     decides; remove it"
                )
            });
        }
        if self.auto_install == Some(false) && self.seed_install.is_none() {
            notes.push(
                "[packages] auto_install = false keeps the ALab toolset from installing \
                 anything unattended — the first-run fill and new members of the set \
                 (before 2026-09-23 it gated only a network bootstrap); remove the line \
                 for the batteries-included default"
                    .to_string(),
            );
        }
        for (key, present) in [
            ("channel", self.channel.is_some()),
            ("include", self.include.is_some()),
        ] {
            if present {
                notes.push(retired_key_note(key));
            }
        }
        notes.extend(self.admission_notes.iter().cloned());
        notes
    }

    /// The narrowing-only exclude filter.
    #[must_use]
    pub fn exclude(&self) -> &[String] {
        self.exclude.as_deref().unwrap_or(&[])
    }
}

/// The line a retired `auto_update` gets, for doctor and the config editor. With no
/// `enabled` beside it the fix is the rename; with `enabled` already there a rename would
/// write the key twice (a file that no longer parses, read as updates on), so the fix is to
/// delete the old key and, when it keeps updates off, say that in `enabled`.
#[must_use]
pub fn auto_update_note(auto_update: bool, enabled: Option<bool>) -> String {
    match (enabled, auto_update) {
        (None, _) => format!(
            "[packages] auto_update = {auto_update} is retired — it is read as \
             `enabled = {auto_update}`; rename it to `enabled` (Settings ▸ Packages writes \
             that key)"
        ),
        (Some(true), false) => "[packages] auto_update = false is retired — it still keeps \
                                automatic updates off; remove it and set `enabled = false` to \
                                keep them off (renaming it would write `enabled` twice)"
            .to_string(),
        (Some(_), _) => format!(
            "[packages] auto_update = {auto_update} is retired and `enabled` decides; remove it"
        ),
    }
}

/// The line a retired `[packages]` key gets: at load (once per process, [`admit_packages`])
/// and from doctor.
fn retired_key_note(key: &str) -> String {
    match key {
        "channel" => "[packages] channel is retired — atpkg reads the one `stable` channel; \
                      remove the key"
            .to_string(),
        _ => format!(
            "[packages] {key} is retired — a machine keeps the whole signed set minus \
             `exclude`; remove the key"
        ),
    }
}

/// What one `[packages.links]` value means, classified fail-closed: only the two
/// sanctioned shapes act; everything else is [`LinkTarget::Invalid`], ignored loudly where
/// it is consumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// A local checkout path (absolute, or `~`-anchored) → managed dev-link.
    Checkout(PathBuf),
    /// A slug-validated `owner/repo` → private-repo fetch override for the program's
    /// release assets (the signed index must still name the program).
    Repo(String),
    /// Neither shape (relative path, `~user`, URL-metacharacter slug, …).
    Invalid,
}

impl PackagesConfig {
    /// The configured install prefix as an absolute path, or `None` for the default.
    ///
    /// `~`/`~/…` expands against `home` (no `~user`). A relative path is `None`: a prefix
    /// that depends on the CWD is not a prefix. No trust check here;
    /// [`crate::store::vet_prefix`] decides, failing closed to the default.
    #[must_use]
    pub fn prefix_path(&self, home: Option<&Path>) -> Option<PathBuf> {
        let v = self.prefix.as_deref()?.trim();
        if v.is_empty() {
            return None;
        }
        if let Some(rest) = v.strip_prefix('~') {
            let home = home?;
            return match rest.strip_prefix('/') {
                Some(tail) => Some(home.join(tail)),
                None if rest.is_empty() => Some(home.to_path_buf()),
                None => None, // `~user` is not supported
            };
        }
        Path::new(v).is_absolute().then(|| PathBuf::from(v))
    }
}

/// Classify one `[packages.links]` value. `home` backs `~` expansion (`None`: a `~` value
/// is Invalid). A repo value must be exactly `owner/repo` with both parts passing
/// [`aterm_update_core::is_valid_slug`], so a malformed value can never redirect a fetch
/// off the GitHub API.
#[must_use]
pub fn classify_link(value: &str, home: Option<&Path>) -> LinkTarget {
    let v = value.trim();
    if let Some(rest) = v.strip_prefix('~') {
        // `~` or `~/…` only — `~user` expansion is not supported (Invalid).
        let Some(home) = home else {
            return LinkTarget::Invalid;
        };
        return match rest.strip_prefix('/') {
            Some(tail) => LinkTarget::Checkout(home.join(tail)),
            None if rest.is_empty() => LinkTarget::Checkout(home.to_path_buf()),
            None => LinkTarget::Invalid,
        };
    }
    if Path::new(v).is_absolute() {
        return LinkTarget::Checkout(PathBuf::from(v));
    }
    if let Some((owner, repo)) = v.split_once('/')
        && aterm_update_core::is_valid_slug(owner)
        && aterm_update_core::is_valid_slug(repo)
    {
        return LinkTarget::Repo(format!("{owner}/{repo}"));
    }
    LinkTarget::Invalid
}

/// The program → `owner/repo` fetch-override map from `[packages.links]` (the
/// [`LinkTarget::Repo`] entries only), consumed by [`crate::GithubFetcher::with_overrides`].
#[must_use]
pub fn repo_overrides(cfg: &PackagesConfig) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (program, value) in &cfg.links {
        // `home: None` is fine: a `~…` value is Checkout or Invalid either way, never Repo.
        if let LinkTarget::Repo(slug) = classify_link(value, None) {
            out.insert(program.clone(), slug);
        }
    }
    out
}

/// The path of the user config file, mirroring the GUI's `app_config.rs::config_path`:
/// `$XDG_CONFIG_HOME/aterm/aterm.toml`, else (Windows) `%APPDATA%\aterm\aterm.toml`, else
/// `$HOME/.config/aterm/aterm.toml`.
#[must_use]
pub fn config_path() -> Option<PathBuf> {
    // `aterm_types::dirs::config_home`: `$XDG_CONFIG_HOME`, or a test binary's own root.
    if let Some(x) = aterm_types::dirs::config_home() {
        return Some(PathBuf::from(x).join("aterm").join("aterm.toml"));
    }
    #[cfg(windows)]
    if let Some(appdata) = std::env::var_os("APPDATA").filter(|a| !a.is_empty()) {
        return Some(PathBuf::from(appdata).join("aterm").join("aterm.toml"));
    }
    // `HOME` first, the account's home second: a process launched without `HOME` (launchd,
    // `su` without `-`, a sandbox) would otherwise read no file and let both `[machine]`
    // defaults act over an explicit `universal_control = "leave"`. `defaults` follows the
    // account regardless of `$HOME`.
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|h| !h.as_os_str().is_empty())
        .or_else(crate::platform::account_home)
        .map(|h| h.join(".config/aterm/aterm.toml"))
}

/// The `[machine]` table: the machine settings `aterm pkg machine apply` applies at first
/// open and keeps applied. atpkg applies them; the GUI only renders the resulting
/// `machine-settings:` row.
///
/// All-Option like [`PackagesConfig`]; defaults live only in the resolver methods. The
/// default acts, so the opt-out is spelled explicitly: `universal_control = "leave"`.
#[derive(Debug, Default, Clone, serde::Deserialize)]
#[serde(default)]
pub struct MachineConfig {
    /// `[machine].universal_control`: `"off"` (default) writes
    /// `com.apple.universalcontrol Disable`/`DisableMagicEdges` for the current host when
    /// they are not already set; `"leave"` never touches them.
    pub universal_control: Option<String>,
    /// The config file exists and could not be parsed, so the opt-out above could not be
    /// read. Never deserialized; [`parse_machine`] sets it. The defaults act, so a table
    /// read from a file nobody could read fails closed.
    #[serde(skip)]
    pub unreadable: bool,
}

/// What the pass does about macOS Universal Control (the cursor/keyboard roaming to
/// other Macs and iPads signed into the same Apple account).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UniversalControlPolicy {
    /// Disable it for this host (both keys), if not already disabled. The default.
    Off,
    /// Do nothing, ever.
    Leave,
}

impl MachineConfig {
    /// The reading a `[machine]` table we could not parse gets: touch nothing, with the
    /// opt-out spelled out. Not [`Self::default`], whose defaults act, so a typo would
    /// re-enable writes the owner opted out of.
    #[must_use]
    pub fn inert() -> Self {
        Self {
            universal_control: Some("leave".to_string()),
            // Inert is not unreadable: `parse_machine` sets that flag itself.
            unreadable: false,
        }
    }

    /// `[machine].universal_control`, default `off`. An unrecognised spelling is `Leave`,
    /// the inert reading, and says so once ([`crate::notice`]).
    #[must_use]
    pub fn universal_control(&self) -> UniversalControlPolicy {
        match self.universal_control.as_deref().map(str::trim) {
            None | Some("") | Some("off") => UniversalControlPolicy::Off,
            Some("leave") => UniversalControlPolicy::Leave,
            Some(other) => {
                crate::notice::say(&format!(
                    "[machine] universal_control = {other:?} is not \"off\" or \"leave\" — \
                     leaving Universal Control alone"
                ));
                UniversalControlPolicy::Leave
            }
        }
    }
}

/// Deserialization wrapper for the `[packages]` table alone; every other table of
/// `aterm.toml` is ignored (the GUI owns that schema). One table per wrapper: a table a
/// wrapper does not name is skipped without being type-checked, so a typo costs only its
/// own table (a `[packages]` type error never hands [`parse_machine`] the acting defaults).
#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct PackagesOnly {
    packages: Option<PackagesConfig>,
}

/// Deserialization wrapper for the `[machine]` table alone, read only by [`parse_machine`].
#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct MachineOnly {
    machine: Option<MachineConfig>,
}

/// The `[reroute]` table — what a session does with the upstream Rust names
/// ([`crate::reroute`]). `announce` silences the stub's line (a setting, not an env
/// var); Settings ▸ Packages writes it. `strict` opts back into refusals instead of
/// warnings; it is written by hand (Manual).
#[derive(Debug, Default, Clone, serde::Deserialize)]
#[serde(default)]
pub struct RerouteConfig {
    /// `[reroute].announce`: whether a stub prints its line before it runs an upstream tool
    /// (a SIGNPOST row such as `cargo`, `rustc`, `tlc`; `z3`; a DIRECT row whose Trust tool
    /// is missing). Default true. `false` silences only that line: a DIRECT row still says
    /// what it substituted, and a refusal is not an announcement.
    pub announce: Option<bool>,
    /// `[reroute].strict`: default false, so every stub runs something with a one-line
    /// notice, except a non-Trust `cargo`/`rustc` inside a Trust source checkout
    /// ([`crate::reroute::trust_checkout_refusal`]). `true` refuses instead: `z3` exits 2, a
    /// DIRECT row whose Trust tool is missing exits 127 instead of running upstream, and a
    /// stub that cannot reach atpkg exits 2 instead of walking PATH.
    pub strict: Option<bool>,
}

impl RerouteConfig {
    /// `[reroute].announce`, default true.
    #[must_use]
    pub fn announce(&self) -> bool {
        self.announce.unwrap_or(true)
    }

    /// `[reroute].strict`, default false.
    #[must_use]
    pub fn strict(&self) -> bool {
        self.strict.unwrap_or(false)
    }
}

/// The `[abrew]` table — aterm's Homebrew ([`crate::abrew`],
/// docs/DESIGN-abrew-2026-10-19.md). `enabled = false` lays no `brew`
/// stub and makes the engine pass through to real Homebrew; the
/// `abrew off` stamp and this setting are two spellings of off, and
/// either one wins (fail toward the user's real brew).
#[derive(Debug, Default, Clone, serde::Deserialize)]
#[serde(default)]
pub struct AbrewConfig {
    /// `[abrew].enabled`: default true.
    pub enabled: Option<bool>,
}

impl AbrewConfig {
    /// `[abrew].enabled`, default true.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
}

/// Deserialization wrapper for the `[abrew]` table alone.
#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct AbrewOnly {
    abrew: Option<AbrewConfig>,
}

/// Parse the `[abrew]` table out of full `aterm.toml` text.
#[must_use]
pub fn parse_abrew(text: &str) -> AbrewConfig {
    match aterm_toml::from_str::<AbrewOnly>(text) {
        Ok(root) => root.abrew.unwrap_or_default(),
        // A malformed document cannot establish the absence of an opt-out.
        // Valid omitted tables still use the default; unknown input does not
        // enable the reroute, regardless of how its keys were spelled.
        Err(_) => AbrewConfig {
            enabled: Some(false),
        },
    }
}

/// The `[abrew]` config as the file says it now (long-lived callers
/// re-read, like [`load_reroute`]).
#[must_use]
pub fn load_abrew() -> AbrewConfig {
    config_path()
        .and_then(|p| read_config_text(&p))
        .map_or_else(AbrewConfig::default, |t| parse_abrew(&t))
}

/// The process-wide `[abrew]` config, read once per invocation.
#[must_use]
pub fn cached_abrew() -> &'static AbrewConfig {
    static CFG: std::sync::OnceLock<AbrewConfig> = std::sync::OnceLock::new();
    CFG.get_or_init(load_abrew)
}

/// Deserialization wrapper for the `[reroute]` table alone (see [`PackagesOnly`]).
#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct RerouteOnly {
    reroute: Option<RerouteConfig>,
}

/// Parse the `[reroute]` table out of full `aterm.toml` text: no table, or one that does
/// not parse, is the default, which announces (a typo must not hide which toolchain a build
/// ran on).
#[must_use]
pub fn parse_reroute(text: &str) -> RerouteConfig {
    aterm_toml::from_str::<RerouteOnly>(text)
        .ok()
        .and_then(|root| root.reroute)
        .unwrap_or_default()
}

/// The process-wide `[reroute]` config, read once per invocation like [`cached`] — a
/// `__reroute` process reads it at most once per stub exec.
#[must_use]
pub fn cached_reroute() -> &'static RerouteConfig {
    static CFG: std::sync::OnceLock<RerouteConfig> = std::sync::OnceLock::new();
    CFG.get_or_init(load_reroute)
}

/// The `[reroute]` config as the file says it now, for a long-lived caller: the window
/// lays the reroute stubs at every session spawn ([`crate::reroute::lay`]) and a stub
/// carries `strict` in its bytes, so a value cached at launch would re-lay a stale policy.
#[must_use]
pub fn load_reroute() -> RerouteConfig {
    config_path()
        .and_then(|p| read_config_text(&p))
        .map_or_else(RerouteConfig::default, |t| parse_reroute(&t))
}

/// Parse the `[packages]` table out of full `aterm.toml` text: no table ⇒ defaults; a
/// malformed file ⇒ the loud unreadable reading ([`PackagesConfig::unreadable_table`],
/// never the plain defaults, two of which widen). A well-formed table then passes
/// [`admit_packages`].
#[must_use]
pub fn parse_packages(text: &str) -> PackagesConfig {
    parse_packages_with(text, DEV_SEAMS)
}

/// [`parse_packages`] with the build's seam posture as an input, so a test pins what a
/// shipped binary makes of the development settings, under the process's home
/// ([`aterm_types::dirs::home_dir`]).
#[must_use]
pub fn parse_packages_with(text: &str, dev_seams: bool) -> PackagesConfig {
    parse_packages_at(text, dev_seams, aterm_types::dirs::home_dir().as_deref())
}

/// [`parse_packages_with`] with `home` explicit.
#[must_use]
pub fn parse_packages_at(text: &str, dev_seams: bool, home: Option<&Path>) -> PackagesConfig {
    match aterm_toml::from_str::<PackagesOnly>(text) {
        Ok(root) => admit_packages(root.packages.unwrap_or_default(), dev_seams, home),
        Err(e) => {
            // Said once per process wherever its unasked notices go ([`crate::notice`]):
            // every layout resolution reads the file again. A file that is not TOML at all
            // is said once for both tables ([`say_not_toml`]); only a shape error is this
            // table's.
            if !say_not_toml(text) {
                crate::notice::say_once(&format!(
                    "[packages] in aterm.toml: {e} \u{2014} updates continue; new programs \
                     wait until it is fixed"
                ));
            }
            PackagesConfig::unreadable_table()
        }
    }
}

/// When `text` is not valid TOML, say so once for every table read from it and answer
/// true: the parser fails the whole file, so a per-table line would blame a table that
/// has nothing wrong with it.
fn say_not_toml(text: &str) -> bool {
    match not_toml_note(text) {
        Some(note) => {
            crate::notice::say_once(&note);
            true
        }
        None => false,
    }
}

/// The one line for an `aterm.toml` that is not valid TOML, or `None` when it parses.
fn not_toml_note(text: &str) -> Option<String> {
    let e = aterm_toml::from_str::<aterm_toml::Value>(text).err()?;
    Some(format!(
        "aterm.toml is not valid TOML ({e}) \u{2014} updates continue; new programs and the \
         [machine] settings wait until it is fixed"
    ))
}

/// The per-key admission a well-formed `[packages]` table gets, once, at load.
///
/// A retired `channel`/`include` is named ([`crate::notice::say_spelling`]) and never read.
/// In a shipped binary (`dev_seams` false) the development settings (`prefix`, `account`,
/// every `owner/repo` value of `[packages.links]`) are named and dropped from the table, so
/// no resolver can act on them; a checkout-path link stays. A dropped `prefix` naming a
/// store other than the default under `home` is kept as
/// [`PackagesConfig::ignored_prefix`], which stops unattended installs until the line is
/// removed.
///
/// Every note is said once per process, never by a process that reports the spelling
/// itself (doctor) or says nothing unasked (`__reroute`) — [`crate::notice`] — since every
/// layout resolution reads this table. What was dropped is also kept in
/// [`PackagesConfig::admission_notes`], which doctor prints.
fn admit_packages(mut cfg: PackagesConfig, dev_seams: bool, home: Option<&Path>) -> PackagesConfig {
    for (key, present) in [
        ("channel", cfg.channel.is_some()),
        ("include", cfg.include.is_some()),
    ] {
        if present {
            crate::notice::say_spelling(&retired_key_note(key));
        }
    }
    if !dev_seams {
        let mut dropped: Vec<String> = Vec::new();
        let configured = cfg.prefix_path(home);
        if let (Some(configured), Some(home)) = (configured, home)
            && configured != crate::store::default_prefix(home)
        {
            cfg.ignored_prefix = Some(configured);
        }
        // An ignored prefix has its own notice (below), naming the key and the remedy.
        if cfg.prefix.take().is_some_and(|p| !p.trim().is_empty()) && cfg.ignored_prefix.is_none() {
            dropped.push("prefix".to_string());
        }
        if cfg.account.take().is_some_and(|a| !a.trim().is_empty()) {
            dropped.push("account".to_string());
        }
        let repo_links: Vec<String> = cfg
            .links
            .iter()
            .filter(|(_, value)| matches!(classify_link(value, None), LinkTarget::Repo(_)))
            .map(|(program, _)| program.clone())
            .collect();
        for program in repo_links {
            cfg.links.remove(&program);
            dropped.push(format!("links.{program}"));
        }
        if !dropped.is_empty() {
            cfg.admission_notes.push(format!(
                "[packages] {} — a development setting this build does not read (it keeps \
                 the one default path); remove it",
                dropped.join(", ")
            ));
        }
    }
    for note in &cfg.admission_notes {
        crate::notice::say_spelling(note);
    }
    // Not a `note`: doctor warns it and Settings ▸ Packages shows it, because it stops
    // unattended installs ([`PackagesConfig::installs_unattended`]).
    if let Some(ignored) = cfg.ignored_prefix.as_deref() {
        crate::notice::say_spelling(&ignored_prefix_note(ignored));
    }
    cfg
}

/// What a person is told about a `[packages] prefix` this build ignores
/// ([`PackagesConfig::ignored_prefix`]) — doctor's warn, Settings ▸ Packages' line and the
/// load-time notice all say these words.
#[must_use]
pub fn ignored_prefix_note(prefix: &Path) -> String {
    format!(
        "[packages] prefix = {} is not used by this build — aterm keeps its packages in \
         the one default store — so nothing is installed automatically until you remove \
         the line (what that prefix holds is left as it is)",
        prefix.display()
    )
}

/// Parse the `[machine]` table out of full `aterm.toml` text: no table ⇒ the defaults,
/// which act. A table we cannot read yields the loud inert reading
/// ([`MachineConfig::inert`]), never the acting defaults.
#[must_use]
pub fn parse_machine(text: &str) -> MachineConfig {
    match aterm_toml::from_str::<MachineOnly>(text) {
        Ok(root) => root.machine.unwrap_or_default(),
        Err(e) => {
            // `inert()` makes every resolver leave the machine alone; `unreadable` lets the
            // CLI verdict, Settings' This Mac status and the `machine-state:` record say why
            // nothing is applied, rather than show a machine deliberately switched off.
            if !say_not_toml(text) {
                crate::notice::say(&format!(
                    "[machine] in aterm.toml: {e} \u{2014} its settings are not applied; \
                     Universal Control is left as it is"
                ));
            }
            MachineConfig {
                unreadable: true,
                ..MachineConfig::inert()
            }
        }
    }
}

/// Load the `[packages]` table from the real config file (absent or unreadable ⇒ defaults,
/// malformed ⇒ the loud unreadable reading via [`parse_packages`]).
#[must_use]
pub fn load() -> PackagesConfig {
    let Some(path) = config_path() else {
        return PackagesConfig::default();
    };
    load_from_path(&path)
}

/// Load the `[machine]` table from the real config file, under [`load`]'s rules.
#[must_use]
pub fn load_machine() -> MachineConfig {
    config_path()
        .and_then(|p| read_config_text(&p))
        .map_or_else(MachineConfig::default, |t| parse_machine(&t))
}

/// The admitted text of the config file, or `None` when absent/unreadable.
fn read_config_text(path: &Path) -> Option<String> {
    // The config file may be a dotfile-manager symlink: follow only that bounded final
    // link chain, then read the regular target through one non-blocking, bounded handle.
    crate::metadata_io::read_bounded_regular_utf8_follow_final_links(
        path,
        MAX_PACKAGES_CONFIG_BYTES,
    )
    .ok()
}

fn load_from_path(path: &Path) -> PackagesConfig {
    // Absent or unreadable ⇒ defaults.
    read_config_text(path).map_or_else(PackagesConfig::default, |t| parse_packages(&t))
}

/// The `[packages]` table of the config file at `path`, read now under [`load`]'s rules,
/// for a long-lived reader that must see an edit (the window's package loop re-reads the
/// Automatic-updates switch before every pass). `None` when the file exists but cannot be
/// read: a transient failure must not read as the defaults.
#[must_use]
pub fn load_live(path: &Path) -> Option<PackagesConfig> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(PackagesConfig::default()),
        Err(_) => None,
        Ok(_) => read_config_text(path).map(|t| parse_packages(&t)),
    }
}

/// The process-wide `[machine]` config, read once per invocation like [`cached`].
#[must_use]
pub fn cached_machine() -> &'static MachineConfig {
    static CFG: std::sync::OnceLock<MachineConfig> = std::sync::OnceLock::new();
    CFG.get_or_init(load_machine)
}

/// The process-wide `[packages]` config, read once per invocation (atpkg is a short-lived
/// CLI).
#[must_use]
pub fn cached() -> &'static PackagesConfig {
    static CFG: std::sync::OnceLock<PackagesConfig> = std::sync::OnceLock::new();
    CFG.get_or_init(load)
}

#[cfg(test)]
mod tests {

    #[test]
    fn abrew_config_fails_closed_on_malformed_table() {
        assert!(!parse_abrew("[abrew]\nenabled = maybe\n").enabled());
        assert!(!parse_abrew("[abrew]\nenabled = false\n").enabled());
        assert!(parse_abrew("[abrew]\nenabled = true\n").enabled());
        assert!(parse_abrew("").enabled());
        for malformed in [
            "[ abrew ]\nenabled = maybe\n",
            "['abrew']\nenabled = maybe\n",
            "[\"abrew\"]\nenabled = maybe\n",
            "abrew.enabled = maybe\n",
            "[packages]\nbroken = [\n",
        ] {
            assert!(!parse_abrew(malformed).enabled());
        }
        // Valid unrelated tables and valid absent options retain the default.
        assert!(parse_abrew("[packages]\nexclude = []\n").enabled());
        assert!(parse_abrew("[ abrew ]\n").enabled());
        assert!(!parse_abrew("['abrew']\nenabled = false\n").enabled());
    }

    use super::*;

    /// `unix` (a rooted `/…` path) as an absolute path on this host: Windows roots it
    /// on a drive, since `/opt/…` alone is drive-relative there.
    fn abs(unix: &str) -> String {
        if cfg!(windows) {
            format!("C:{unix}")
        } else {
            unix.to_string()
        }
    }

    // `[machine]`: the default acts (Universal Control off); the opt-out is the explicit
    // spelling; a word the owner did not write is inert.
    #[test]
    fn machine_table_defaults_act_and_opt_outs_are_explicit() {
        let none = parse_machine("font_px = 12.0\n[packages]\nexclude = [\"trust\"]\n");
        assert_eq!(none.universal_control(), UniversalControlPolicy::Off);
        let off = parse_machine("[machine]\nuniversal_control = \"leave\"\n");
        assert_eq!(off.universal_control(), UniversalControlPolicy::Leave);
        let explicit = parse_machine("[machine]\nuniversal_control = \"off\"\n");
        assert_eq!(explicit.universal_control(), UniversalControlPolicy::Off);
        assert_eq!(
            parse_machine("[machine]\nuniversal_control = \" OFF \"\n").universal_control(),
            UniversalControlPolicy::Leave,
            "an unknown spelling never acts"
        );
        let unreadable = parse_machine("[machine]\nuniversal_control = 1\n");
        assert_eq!(
            unreadable.universal_control(),
            UniversalControlPolicy::Leave,
            "a [machine] table that does not parse falls to the INERT reading, loudly: \
             these defaults ACT, and unreadable words are no licence to write the host"
        );
        // The two tables come out of one file, independently.
        let both = "[packages]\nexclude = [\"ay\"]\n[machine]\nuniversal_control = \"leave\"\n";
        assert_eq!(parse_packages(both).exclude(), ["ay".to_string()]);
        assert_eq!(
            parse_machine(both).universal_control(),
            UniversalControlPolicy::Leave
        );
    }

    /// A typo in `[packages]` never re-arms the acting `[machine]` defaults: each table is
    /// parsed on its own, and an unreadable `[machine]` table falls to the inert reading.
    #[test]
    fn a_packages_typo_never_re_arms_the_machine_defaults() {
        let typo = "[packages]\nauto_install = \"yes\"\n[machine]\n\
                    universal_control = \"leave\"\n";
        let m = parse_machine(typo);
        assert_eq!(
            m.universal_control(),
            UniversalControlPolicy::Leave,
            "a [packages] type error must not cancel an explicit universal_control opt-out"
        );
        // The mirror: a `[machine]` type error must not silence the `[packages]` table.
        let other = "[packages]\nexclude = [\"ay\"]\n[machine]\nuniversal_control = 3\n";
        assert_eq!(
            parse_packages(other).exclude(),
            ["ay".to_string()],
            "a [machine] type error must not discard the [packages] table"
        );
        // A file that is not TOML at all: nothing readable said to act, so nothing acts.
        let broken = parse_machine("[machine\nnot toml");
        assert_eq!(broken.universal_control(), UniversalControlPolicy::Leave);
        // A syntax error outside both tables is the file's, said once, never blamed on
        // [packages]; a well-formed file has no such line.
        let note = not_toml_note("[packages]\nexclude = [\"ay\"]\n[window]\ntitle = \"x\n")
            .expect("an unclosed quote is not TOML");
        assert!(note.starts_with("aterm.toml is not valid TOML"), "{note}");
        assert!(!note.contains("[packages] in"), "{note}");
        assert_eq!(not_toml_note("[machine]\nuniversal_control = 1\n"), None);
    }

    #[test]
    fn absent_table_yields_the_batteries_included_defaults() {
        let cfg = parse_packages("font_px = 12.0\n[matrix_rain]\nenabled = true\n");
        assert_eq!(cfg.account(), None);
        assert!(cfg.enabled(), "automatic updates default ON");
        assert!(
            cfg.auto_install(),
            "the one install consent defaults ON (batteries included, §9.1 — installing \
             aterm is wanting the toolset)"
        );
        assert!(cfg.exclude().is_empty());
        assert!(cfg.links.is_empty());
        assert!(cfg.config_notes().is_empty(), "nothing to rename");
        assert_eq!(cfg.enabled, None);
        assert_eq!(cfg.auto_update, None);
    }

    #[test]
    fn full_table_parses_every_key() {
        let cfg = parse_packages_with(
            "[packages]\nenabled = true\nauto_install = false\n\
             account = \"alabsystems\"\nexclude = [\"trust\"]\n\
             [packages.links]\nay = \"~/ay\"\norc = \"alabsystems/orc\"\n",
            true,
        );
        assert_eq!(cfg.enabled, Some(true));
        assert!(cfg.enabled());
        assert!(!cfg.auto_install());
        assert_eq!(cfg.account(), Some("alabsystems"));
        assert_eq!(cfg.exclude(), ["trust".to_string()]);
        assert_eq!(cfg.links.get("ay").map(String::as_str), Some("~/ay"));
        assert_eq!(
            cfg.links.get("orc").map(String::as_str),
            Some("alabsystems/orc")
        );
    }

    /// `auto_update` is folded into `enabled`: its `false` still reads as off, whatever
    /// `enabled` says, and doctor names the rename; `true` changes nothing.
    #[test]
    fn a_retired_auto_update_false_still_switches_automatic_updates_off() {
        let old = parse_packages("[packages]\nauto_update = false\n");
        assert!(
            !old.enabled(),
            "an existing `auto_update = false` is honoured"
        );
        assert!(
            old.config_notes()
                .iter()
                .any(|n| n.contains("auto_update") && n.contains("enabled = false")),
            "{:?}",
            old.config_notes()
        );
        let both = parse_packages("[packages]\nenabled = true\nauto_update = false\n");
        assert!(
            !both.enabled(),
            "the old off switch stands until the file is rewritten (Settings drops it)"
        );
        assert!(parse_packages("[packages]\nauto_update = true\n").enabled());
        assert!(!parse_packages("[packages]\nenabled = false\n").enabled());
    }

    /// The rename is said only where a rename parses: with `enabled` already written,
    /// renaming `auto_update` would write a duplicate key, a file atpkg reads as automatic
    /// updates on.
    #[test]
    fn the_auto_update_note_never_asks_for_a_duplicate_key() {
        let both = parse_packages("[packages]\nenabled = true\nauto_update = false\n");
        let notes = both.config_notes();
        assert!(
            notes.iter().all(|n| !n.contains("rename")),
            "a rename would write `enabled` twice: {notes:?}"
        );
        assert!(
            notes
                .iter()
                .any(|n| n.contains("remove it and set `enabled = false`")),
            "{notes:?}"
        );
        // Following the advice keeps updates off and the file readable.
        let followed = parse_packages("[packages]\nenabled = false\n");
        assert!(!followed.unreadable && !followed.enabled());
        // Both off, or the old key on: `enabled` decides, and the old key just goes.
        for text in [
            "[packages]\nenabled = false\nauto_update = false\n",
            "[packages]\nenabled = true\nauto_update = true\n",
        ] {
            let notes = parse_packages(text).config_notes();
            assert!(
                notes
                    .iter()
                    .any(|n| n.ends_with("`enabled` decides; remove it")),
                "{text}: {notes:?}"
            );
        }
        // Alone, it is still the rename.
        assert!(
            parse_packages("[packages]\nauto_update = false\n")
                .config_notes()
                .iter()
                .any(|n| n.contains("rename it to `enabled`"))
        );
    }

    /// A prefix this build ignores stops unattended installs: a shipped binary drops a
    /// `[packages] prefix` naming another store, and until the line goes the seed and the
    /// set completion install nothing. One naming the default, and every development build,
    /// change nothing.
    #[test]
    fn an_ignored_prefix_stops_unattended_installs_until_the_line_is_removed() {
        let home = Path::new("/Users//someone");
        let opt = abs("/opt/aterm/pkg");
        let shared = parse_packages_at(
            &format!("[packages]\nprefix = \"{opt}\"\n"),
            false,
            Some(home),
        );
        assert_eq!(shared.prefix, None, "dropped");
        assert_eq!(shared.ignored_prefix.as_deref(), Some(Path::new(&opt)));
        assert!(
            shared.auto_install(),
            "the consent itself is untouched (Settings shows it)"
        );
        assert!(!shared.installs_unattended(), "no second store is filled");
        assert!(
            ignored_prefix_note(Path::new("/opt/aterm/pkg")).contains("remove the line"),
            "the note names the fix"
        );
        assert!(
            !shared.config_notes().iter().any(|n| n.contains("prefix")),
            "the ignored-prefix note alone says it, not a second drop note: {:?}",
            shared.config_notes()
        );
        let default = crate::store::default_prefix(home);
        let same = parse_packages_at(
            &format!("[packages]\nprefix = {:?}\n", default.display().to_string()),
            false,
            Some(home),
        );
        assert_eq!(
            same.ignored_prefix, None,
            "the default named is the default used"
        );
        assert!(same.installs_unattended());
        let dev = parse_packages_at(
            &format!("[packages]\nprefix = \"{opt}\"\n"),
            true,
            Some(home),
        );
        assert_eq!(dev.ignored_prefix, None, "a development build honours it");
        assert!(dev.installs_unattended());
        let removed = parse_packages_at("[packages]\n", false, Some(home));
        assert!(
            removed.installs_unattended(),
            "removing the line is the whole fix"
        );
    }

    /// One install consent: `auto_install` wins when written, else the retired
    /// `seed_install`, else on; a table we could not read is off.
    #[test]
    fn one_install_consent_migrates_both_retired_spellings() {
        let consent = |toml: &str| parse_packages(toml).auto_install();
        assert!(consent(""));
        assert!(
            !consent("[packages]\nseed_install = false\n"),
            "`not on my disk` stands"
        );
        assert!(consent("[packages]\nseed_install = true\n"));
        assert!(
            consent("[packages]\nseed_install = false\nauto_install = true\n"),
            "the old explicit network-bootstrap consent still installs"
        );
        assert!(
            !consent("[packages]\nseed_install = true\nauto_install = false\n"),
            "auto_install decides when written"
        );
        assert!(!consent("[packages]\nauto_install = false\n"));
        // Doctor names each retired spelling once.
        let notes = parse_packages("[packages]\nseed_install = false\n").config_notes();
        assert!(
            notes
                .iter()
                .any(|n| n.contains("seed_install") && n.contains("auto_install = false")),
            "{notes:?}"
        );
        let notes = parse_packages("[packages]\nauto_install = false\n").config_notes();
        assert!(
            notes.iter().any(|n| n.contains("unattended")),
            "an explicit auto_install = false is told what it now means: {notes:?}"
        );
    }

    /// `channel` and `include` are retired: parsed as any value (a stale key never fails
    /// the table), never read, and named by doctor.
    #[test]
    fn retired_channel_and_include_are_ignored_and_named() {
        let cfg = parse_packages(
            "[packages]\nchannel = \"nightly\"\ninclude = [\"ay\"]\nexclude = [\"trust\"]\n",
        );
        assert!(
            !cfg.unreadable,
            "a retired key does not make the table unreadable"
        );
        assert_eq!(
            cfg.exclude(),
            ["trust".to_string()],
            "the rest of the table reads"
        );
        let notes = cfg.config_notes();
        assert!(
            notes.iter().any(|n| n.contains("channel is retired")),
            "{notes:?}"
        );
        assert!(
            notes.iter().any(|n| n.contains("include is retired")),
            "{notes:?}"
        );
        assert!(
            !parse_packages("[packages]\nchannel = 7\n").unreadable,
            "any value type is admitted for a retired key"
        );
    }

    /// A shipped binary drops `prefix`, `account` and every `owner/repo` link at load; a
    /// checkout link stays. A development build keeps them all.
    #[test]
    fn a_shipped_binary_drops_the_development_settings() {
        let text = "[packages]\nprefix = \"/opt/pkg\"\naccount = \"fork-org\"\n\
                    [packages.links]\nay = \"/src/ay\"\norc = \"fork-org/orc\"\n";
        let shipped = parse_packages_with(text, false);
        assert_eq!(shipped.prefix, None);
        assert_eq!(shipped.account(), None);
        assert_eq!(
            crate::resolve_account(shipped.account()).owner,
            aterm_update_core::ATPKG_INDEX_OWNER
        );
        assert!(
            repo_overrides(&shipped).is_empty(),
            "no fetch override ships"
        );
        assert_eq!(
            shipped.links.get("ay").map(String::as_str),
            Some("/src/ay"),
            "a checkout dev link is kept"
        );
        let dev = parse_packages_with(text, true);
        assert_eq!(dev.prefix.as_deref(), Some("/opt/pkg"));
        assert_eq!(crate::resolve_account(dev.account()).owner, "fork-org");
        assert_eq!(repo_overrides(&dev).len(), 1);
        // An invalid account never redirects, in either posture.
        let bad = parse_packages_with("[packages]\naccount = \"evil.com/x\"\n", true);
        assert_eq!(
            crate::resolve_account(bad.account()).owner,
            aterm_update_core::ATPKG_INDEX_OWNER
        );
    }

    // Malformed config ⇒ the loud unreadable reading, never a panic.
    #[test]
    fn malformed_config_falls_back_to_defaults() {
        let cfg = parse_packages("[packages\nnot toml");
        assert!(
            !cfg.auto_install(),
            "an unreadable table installs nothing new"
        );
        assert!(cfg.enabled(), "what is installed keeps updating");
        assert!(cfg.links.is_empty());
        assert!(cfg.unreadable, "the table was not read");
        // A [packages] table of the wrong shape (a scalar) is unreadable too.
        let cfg = parse_packages("packages = 3\n");
        assert!(cfg.exclude().is_empty());
        assert!(cfg.unreadable);
    }

    /// A `[packages]` table we could not read must not widen: the install consent defaults
    /// true and `exclude` empty, so its defaults would switch on the multi-GB paths those
    /// keys block. A syntax error anywhere in `aterm.toml` (mostly the GUI's) fails it too.
    #[test]
    fn an_unreadable_packages_table_never_widens_the_download_gates() {
        // A wrong type inside [packages] fails the whole table, `auto_install = false`
        // included.
        let typed = parse_packages("[packages]\nauto_install = false\nenabled = \"yes\"\n");
        assert!(typed.unreadable);
        assert!(
            !typed.auto_install(),
            "a table we could not read is not consent to lay the toolset down: the \
             resolver's TRUE default is the expensive one, and on a lean install it is a \
             multi-GB download the owner declined in writing"
        );
        // A TOML syntax error in the GUI-owned part of the file: [packages] itself is
        // spotless and still unreadable, because the document never parses.
        let elsewhere = parse_packages("[packages]\nauto_install = false\n[machine\nbroken\n");
        assert!(elsewhere.unreadable);
        assert!(!elsewhere.auto_install());
        // `exclude`, the other widening default, cannot be recovered, so the set-completion
        // lane reads the flag instead (`cli::should_complete_set`).
        let excluded = parse_packages("[packages]\nexclude = [\"trust\"]\nenabled = 7\n");
        assert!(excluded.unreadable);
        assert!(
            excluded.exclude().is_empty(),
            "the exclusions are genuinely gone — which is exactly why the pass that \
             would act on their absence must not run"
        );
        // A readable table is untouched by any of this: the default stays true (§9.1).
        let fine = parse_packages("[packages]\nexclude = []\n");
        assert!(!fine.unreadable);
        assert!(fine.auto_install());
        // A hand-built table (never parsed) is readable by construction.
        assert!(!PackagesConfig::default().unreadable);
        assert!(PackagesConfig::default().auto_install());
        assert!(PackagesConfig::unreadable_table().unreadable);
        assert!(!PackagesConfig::unreadable_table().auto_install());
    }

    #[cfg(unix)]
    #[test]
    fn config_fifo_returns_defaults_and_symlinked_config_remains_supported() {
        use std::os::unix::ffi::OsStrExt as _;

        let root = crate::testdir::dir("atpkg-config-admission-");
        let fifo = root.join("fifo.toml");
        let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: `fifo_c` is a live NUL-terminated path in our private fixture.
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        assert!(
            load_from_path(&fifo).exclude().is_empty(),
            "a writerless config FIFO must return the finite default immediately"
        );

        let target = root.join("target.toml");
        let logical = root.join("aterm.toml");
        std::fs::write(&target, "[packages]\nexclude = [\"ay\"]\n").unwrap();
        std::os::unix::fs::symlink(&target, &logical).unwrap();
        assert_eq!(load_from_path(&logical).exclude(), ["ay".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn oversized_sparse_config_returns_bounded_defaults() {
        let root = crate::testdir::dir("atpkg-config-oversized-");
        let path = root.join("aterm.toml");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len((MAX_PACKAGES_CONFIG_BYTES + 1) as u64)
            .unwrap();
        assert!(load_from_path(&path).exclude().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `[reroute].announce` defaults on, `false` silences, and a table that does not parse
    /// announces.
    #[test]
    fn reroute_announce_defaults_on_and_a_typo_announces() {
        assert!(parse_reroute("").announce());
        assert!(parse_reroute("[reroute]\nannounce = true\n").announce());
        assert!(!parse_reroute("[reroute]\nannounce = false\n").announce());
        assert!(parse_reroute("[reroute]\nannounce = \"no\"\n").announce());
        assert!(
            !parse_reroute("[packages]\nenabled = 3\n[reroute]\nannounce = false\n").announce(),
            "a typo in another table does not repeal it"
        );
    }

    /// `[reroute].strict` defaults off (warn and run); only an explicit `true` refuses, and
    /// a value that does not parse is the default.
    #[test]
    fn reroute_strict_defaults_off_and_only_true_turns_it_on() {
        assert!(!parse_reroute("").strict());
        assert!(!parse_reroute("[reroute]\nannounce = false\n").strict());
        assert!(!parse_reroute("[reroute]\nstrict = false\n").strict());
        assert!(parse_reroute("[reroute]\nstrict = true\n").strict());
        assert!(!parse_reroute("[reroute]\nstrict = \"yes\"\n").strict());
        let both = parse_reroute("[reroute]\nannounce = false\nstrict = true\n");
        assert!(
            both.strict() && !both.announce(),
            "the two keys are independent"
        );
    }

    #[test]
    fn classify_link_covers_the_three_shapes() {
        let home = Path::new("/home/u");
        // Absolute path → checkout.
        assert_eq!(
            classify_link(&abs("/src/ay"), Some(home)),
            LinkTarget::Checkout(PathBuf::from(abs("/src/ay")))
        );
        // ~ expansion (against the injected home).
        assert_eq!(
            classify_link("~/ay", Some(home)),
            LinkTarget::Checkout(PathBuf::from("/home/u").join("ay"))
        );
        assert_eq!(
            classify_link("~", Some(home)),
            LinkTarget::Checkout(PathBuf::from("/home/u"))
        );
        // ~ with no resolvable home fails closed, and ~user is unsupported.
        assert_eq!(classify_link("~/ay", None), LinkTarget::Invalid);
        assert_eq!(classify_link("~bob/ay", Some(home)), LinkTarget::Invalid);
        // owner/repo → validated fetch override.
        assert_eq!(
            classify_link("alabsystems/orc", Some(home)),
            LinkTarget::Repo("alabsystems/orc".into())
        );
        // Everything else is Invalid: relative paths, deep slugs, URL metacharacters.
        assert_eq!(classify_link("src/ay/x", Some(home)), LinkTarget::Invalid);
        assert_eq!(
            classify_link("evil.com?x/y", Some(home)),
            LinkTarget::Invalid
        );
        assert_eq!(classify_link("a b/repo", Some(home)), LinkTarget::Invalid);
        assert_eq!(classify_link("", Some(home)), LinkTarget::Invalid);
    }

    #[test]
    fn repo_overrides_extracts_only_validated_slug_entries() {
        let cfg = parse_packages_with(
            "[packages.links]\nay = \"/src/ay\"\norc = \"alabsystems/orc\"\n\
             bad = \"evil host/x\"\n",
            true,
        );
        let map = repo_overrides(&cfg);
        assert_eq!(map.len(), 1, "only the validated owner/repo entry survives");
        assert_eq!(map.get("orc").map(String::as_str), Some("alabsystems/orc"));
    }

    /// `[packages].prefix` shapes; whether a prefix is safe is `store::vet_prefix`'s call.
    #[test]
    fn prefix_path_resolves_absolute_and_tilde_and_refuses_relative() {
        let home = Path::new("/home/someone");
        let of = |v: Option<&str>| {
            PackagesConfig {
                prefix: v.map(str::to_string),
                ..PackagesConfig::default()
            }
            .prefix_path(Some(home))
        };

        assert_eq!(of(None), None, "absent ⇒ the default prefix");
        assert_eq!(of(Some("   ")), None, "blank ⇒ the default prefix");
        assert_eq!(
            of(Some(&abs("/opt/aterm/pkg"))),
            Some(PathBuf::from(abs("/opt/aterm/pkg"))),
            "an absolute system prefix survives to vet_prefix"
        );
        assert_eq!(
            of(Some("~/pkgs")),
            Some(home.join("pkgs")),
            "`~/…` expands against home"
        );
        assert_eq!(of(Some("~")), Some(home.to_path_buf()), "bare `~` is home");
        assert_eq!(of(Some("~root/pkg")), None, "`~user` is not supported");
        assert_eq!(
            of(Some("relative/pkg")),
            None,
            "a CWD-dependent prefix is not a prefix"
        );
    }
}
