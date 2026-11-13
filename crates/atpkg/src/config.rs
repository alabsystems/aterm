// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! atpkg's reader for its tables of the same `aterm.toml` the GUI reads (§11): `[packages]`, `[machine]` and `[reroute]`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Maximum `aterm.toml` size read, matching the native config service's 512 KiB budget.
pub const MAX_PACKAGES_CONFIG_BYTES: usize = aterm_toolchain::discovery::MAX_ATERM_CONFIG_BYTES;

/// The one channel whose pin set drives install/update.
pub const CHANNEL: &str = "stable";

/// Whether this build honours the development settings of `[packages]` (`prefix`, `account`, the `owner/repo` form of `[packages.links]`): only a development build does (`debug_assertions`, or the `dev-seams` feature the release cutter never enables).
pub const DEV_SEAMS: bool = cfg!(any(debug_assertions, feature = "dev-seams"));

/// The `[packages]` table.
#[derive(Debug, Default, Clone, serde::Deserialize)]
#[serde(default)]
pub struct PackagesConfig {
    /// `[packages].prefix`: the install prefix override; absent ⇒ [`crate::store::default_prefix`].
    pub prefix: Option<String>,
    /// `[packages].enabled`: Settings ▸ Packages ▸ "Keep ALab tools up to date" ([`crate::adoption::UPDATES_SWITCH`]), read through [`Self::enabled`].
    pub enabled: Option<bool>,
    /// Retired, folded into `enabled`: `auto_update = false` is still honoured ([`Self::enabled`]) and doctor names the rename; Settings drops this key when it writes `enabled`.
    pub auto_update: Option<bool>,
    /// `[packages].auto_install`: the install consent (Settings ▸ Packages ▸ "Install ALab tools", [`crate::adoption::INSTALLS_SWITCH`]), read through [`Self::auto_install`].
    pub auto_install: Option<bool>,
    /// Retired, folded into `auto_install`: `seed_install = false` is still honoured when `auto_install` is unset ([`Self::auto_install`]), and doctor names the rename.
    pub seed_install: Option<bool>,
    /// Index owner override (e.g. `"alabsystems"`); default the compiled owner ([`crate::discovery::resolve_account`]).
    pub account: Option<String>,
    /// Retired ([`CHANNEL`]): parsed as any value so a stale key never fails the table, named once at load, never read.
    pub channel: Option<aterm_toml::Value>,
    /// Retired: the narrowing include filter (a machine keeps the whole signed set minus `exclude`).
    pub include: Option<aterm_toml::Value>,
    /// Narrowing-only exclude filter: the per-program opt-out.
    pub exclude: Option<Vec<String>>,
    /// `[packages].peers`: LAN peers (`host` or `host:port`) a toolset archive download races,
    /// each unit taken from a peer only when its bytes hash to the signed digest
    /// ([`crate::peers`]). Unwritten, this machine's fleet members ([`crate::peers::unit_peers`]);
    /// `peers = []`, none.
    pub peers: Option<Vec<String>>,
    /// `[packages.links]`: `name = "/path/to/checkout"` (or `~/…`) declares a managed dev link ([`crate::linkmode`]: registry management skipped; never in Settings); `name = "owner/repo"` declares a private-repo fetch override for that program's release assets (verification unchanged), a development setting ([`DEV_SEAMS`]).
    pub links: BTreeMap<String, String>,
    /// Not a config key (`#[serde(skip)]`, so no `aterm.toml` can set it): true only of the table [`parse_packages`] could not read ([`PackagesConfig::unreadable_table`]).
    #[serde(skip)]
    pub unreadable: bool,
    /// Not a config key (`#[serde(skip)]`): the `[packages] prefix` a shipped binary dropped at load ([`admit_packages`]) because it names a store other than the default, resolved against the home it was read under.
    #[serde(skip)]
    pub ignored_prefix: Option<PathBuf>,
    /// Not a config key (`#[serde(skip)]`): the development settings [`admit_packages`] dropped, in its own words.
    #[serde(skip)]
    pub admission_notes: Vec<String>,
}

impl PackagesConfig {
    /// The reading of a `[packages]` table that failed to parse (the twin of [`MachineConfig::inert`]): no key is invented; the flag makes the widening resolvers fail closed.
    #[must_use]
    pub fn unreadable_table() -> Self {
        Self {
            unreadable: true,
            ..Self::default()
        }
    }
    /// Keep ALab tools up to date: `[packages].enabled`, default true, and off wherever the retired `auto_update = false` stands.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(true) && self.auto_update != Some(false)
    }

    /// The `[packages].account` override for [`crate::discovery::resolve_account`] (`None` ⇒ compiled default; a shipped binary never has one — [`DEV_SEAMS`]).
    #[must_use]
    pub fn account(&self) -> Option<&str> {
        self.account
            .as_deref()
            .map(str::trim)
            .filter(|a| !a.is_empty())
    }

    /// The install consent: whether atpkg may install toolset members nobody named (the first-run `seed`, and what the signed default set gains).
    #[must_use]
    pub fn auto_install(&self) -> bool {
        !self.unreadable && self.auto_install.or(self.seed_install).unwrap_or(true)
    }

    /// Whether atpkg may install anything nobody named, here and now: the install consent ([`Self::auto_install`]) and no ignored prefix ([`Self::ignored_prefix`]), read by the first-run seed and the pass's set completion.
    #[must_use]
    pub fn installs_unattended(&self) -> bool {
        self.auto_install() && self.ignored_prefix.is_none()
    }

    /// What doctor says about this table's spelling, one line per key to rename or remove: the retired `auto_update`/`seed_install` (still honoured) and `channel`/`include` (ignored), plus what [`admit_packages`] dropped.
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

/// The line a retired `auto_update` gets, for doctor and the config editor.
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

/// The line a retired `[packages]` key gets: at load (once per process, [`admit_packages`]) and from doctor.
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

/// What one `[packages.links]` value means, classified fail-closed: only the two sanctioned shapes act; everything else is [`LinkTarget::Invalid`], ignored loudly where it is consumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// A local checkout path (absolute, or `~`-anchored) → managed dev-link.
    Checkout(PathBuf),
    /// A slug-validated `owner/repo` → private-repo fetch override for the program's release assets (the signed index must still name the program).
    Repo(String),
    /// Neither shape (relative path, `~user`, URL-metacharacter slug, …).
    Invalid,
}

impl PackagesConfig {
    /// The development checkout selected for one program, independent of whether
    /// its link has already been materialized in the package store.
    #[must_use]
    pub(crate) fn checkout_path(&self, program: &str, home: Option<&Path>) -> Option<PathBuf> {
        match classify_link(self.links.get(program)?, home) {
            LinkTarget::Checkout(path) => Some(path),
            LinkTarget::Repo(_) | LinkTarget::Invalid => None,
        }
    }

    /// The configured install prefix as an absolute path, or `None` for the default.
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
                None => None,
            };
        }
        Path::new(v).is_absolute().then(|| PathBuf::from(v))
    }
}

/// Classify one `[packages.links]` value.
#[must_use]
pub fn classify_link(value: &str, home: Option<&Path>) -> LinkTarget {
    let v = value.trim();
    if let Some(rest) = v.strip_prefix('~') {
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

/// The program → `owner/repo` fetch-override map from `[packages.links]` (the [`LinkTarget::Repo`] entries only), consumed by [`crate::GithubFetcher::with_overrides`].
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

/// The path of the user config file, mirroring the GUI's `app_config.rs::config_path`: `$XDG_CONFIG_HOME/aterm/aterm.toml`, else (Windows) `%APPDATA%\aterm\aterm.toml`, else `$HOME/.config/aterm/aterm.toml`.
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
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|h| !h.as_os_str().is_empty())
        .or_else(crate::platform::account_home)
        .map(|h| h.join(".config/aterm/aterm.toml"))
}

/// The `[machine]` table: the machine settings `aterm pkg machine apply` applies at first open and keeps applied.
#[derive(Debug, Default, Clone, serde::Deserialize)]
#[serde(default)]
pub struct MachineConfig {
    /// `[machine].universal_control`: `"off"` (default) writes `com.apple.universalcontrol Disable`/`DisableMagicEdges` for the current host when they are not already set; `"leave"` never touches them.
    pub universal_control: Option<String>,
    /// `[machine].rosetta`: `true` (default) installs Rosetta 2 on an Apple-silicon Mac that lacks it ([`crate::machine::apply_rosetta`], as the user, no elevation); `false` never does.
    pub rosetta: Option<bool>,
    /// The config file exists and could not be parsed, so the opt-out above could not be read.
    #[serde(skip)]
    pub unreadable: bool,
}

/// What the pass does about macOS Universal Control (the cursor/keyboard roaming to other Macs and iPads signed into the same Apple account).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UniversalControlPolicy {
    /// Disable it for this host (both keys), if not already disabled.
    Off,
    /// Do nothing, ever.
    Leave,
}

impl MachineConfig {
    /// The reading a `[machine]` table we could not parse gets: touch nothing, with the opt-out spelled out.
    #[must_use]
    pub fn inert() -> Self {
        Self {
            universal_control: Some("leave".to_string()),
            rosetta: Some(false),
            unreadable: false,
        }
    }

    /// `[machine].rosetta`, default `true`: whether the apply installs Rosetta 2 where it can need it (macOS on Apple silicon, [`crate::machine::ROSETTA_APPLIES`]).
    #[must_use]
    pub fn rosetta(&self) -> bool {
        self.rosetta.unwrap_or(true)
    }

    /// `[machine].universal_control`, default `off`.
    #[must_use]
    pub fn universal_control(&self) -> UniversalControlPolicy {
        match self.universal_control.as_deref().map(str::trim) {
            None | Some("") | Some("off") => UniversalControlPolicy::Off,
            Some("leave") => UniversalControlPolicy::Leave,
            Some(other) => {
                // A host's log keeps the value's length: it is the person's aterm.toml.
                let tail = "is not \"off\" or \"leave\" — leaving Universal Control alone";
                crate::notice::say_with_log(
                    &format!("[machine] universal_control = {other:?} {tail}"),
                    &format!(
                        "[machine] universal_control = {} {tail}",
                        aterm_log::words::Len(other)
                    ),
                );
                UniversalControlPolicy::Leave
            }
        }
    }
}

/// Deserialization wrapper for the `[packages]` table alone; every other table of `aterm.toml` is ignored (the GUI owns that schema).
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

/// The `[reroute]` table — what a session does with the upstream Rust names ([`crate::reroute`]).
#[derive(Debug, Default, Clone, serde::Deserialize)]
#[serde(default)]
pub struct RerouteConfig {
    /// `[reroute].announce`: whether a stub prints its line before it runs an upstream tool (a SIGNPOST row such as `cargo`, `rustc`, `tlc`; `z3`; a DIRECT row whose Trust tool is missing).
    pub announce: Option<bool>,
    /// `[reroute].strict`: default false, so every stub runs something with a one-line notice, except a non-Trust `cargo`/`rustc` inside a Trust source checkout ([`crate::reroute::trust_checkout_refusal`]).
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

/// The `[abrew]` table — aterm's Homebrew ([`crate::abrew`]).
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
        Err(_) => AbrewConfig {
            enabled: Some(false),
        },
    }
}

/// The `[abrew]` config as the file says it now (long-lived callers re-read, like [`load_reroute`]).
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

/// Deserialization wrapper for the top-level `agents_auto_prime` key alone.
#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct AutoPrimeOnly {
    agents_auto_prime: Option<bool>,
}

/// The top-level `agents_auto_prime` key of full `aterm.toml` text, as the window resolves it (aterm-gui's `agents_auto_prime_from_text`): default true, and a file that does not parse, or a value that is not a boolean, reads as the default.
#[must_use]
pub fn parse_agents_auto_prime(text: &str) -> bool {
    aterm_toml::from_str::<AutoPrimeOnly>(text)
        .ok()
        .and_then(|root| root.agents_auto_prime)
        .unwrap_or(true)
}

/// [`parse_agents_auto_prime`] of the config file as it is now; no file is the default.
#[must_use]
pub fn load_agents_auto_prime() -> bool {
    config_path()
        .and_then(|p| read_config_text(&p))
        .is_none_or(|t| parse_agents_auto_prime(&t))
}

/// Deserialization wrapper for the `[reroute]` table alone (see [`PackagesOnly`]).
#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct RerouteOnly {
    reroute: Option<RerouteConfig>,
}

/// Parse the `[reroute]` table out of full `aterm.toml` text: no table, or one that does not parse, is the default, which announces (a typo must not hide which toolchain a build ran on).
#[must_use]
pub fn parse_reroute(text: &str) -> RerouteConfig {
    aterm_toml::from_str::<RerouteOnly>(text)
        .ok()
        .and_then(|root| root.reroute)
        .unwrap_or_default()
}

/// The process-wide `[reroute]` config, read once per invocation like [`cached`] — a `__reroute` process reads it at most once per stub exec.
#[must_use]
pub fn cached_reroute() -> &'static RerouteConfig {
    static CFG: std::sync::OnceLock<RerouteConfig> = std::sync::OnceLock::new();
    CFG.get_or_init(load_reroute)
}

/// The `[reroute]` config as the file says it now, for a long-lived caller: the window lays the reroute stubs at every session spawn ([`crate::reroute::lay`]) and a stub carries `strict` in its bytes, so a value cached at launch would re-lay a stale policy.
#[must_use]
pub fn load_reroute() -> RerouteConfig {
    config_path()
        .and_then(|p| read_config_text(&p))
        .map_or_else(RerouteConfig::default, |t| parse_reroute(&t))
}

/// Parse the `[packages]` table out of full `aterm.toml` text: no table ⇒ defaults; a malformed file ⇒ the loud unreadable reading ([`PackagesConfig::unreadable_table`], never the plain defaults, two of which widen).
#[must_use]
pub fn parse_packages(text: &str) -> PackagesConfig {
    parse_packages_with(text, DEV_SEAMS)
}

/// [`parse_packages`] with the build's seam posture as an input, so a test pins what a shipped binary makes of the development settings, under the process's home ([`aterm_types::dirs::home_dir`]).
#[must_use]
pub fn parse_packages_with(text: &str, dev_seams: bool) -> PackagesConfig {
    parse_packages_at(text, dev_seams, aterm_types::dirs::home_dir().as_deref())
}

/// [`parse_packages_with`] with `home` explicit.
#[must_use]
pub fn parse_packages_at(text: &str, dev_seams: bool, home: Option<&Path>) -> PackagesConfig {
    match aterm_toml::from_str::<PackagesOnly>(text) {
        Ok(root) => admit_packages(root.packages.unwrap_or_default(), dev_seams, home, true),
        Err(e) => {
            // Said once per process wherever its unasked notices go ([`crate::notice`]):
            // every layout resolution reads the file again. A file that is not TOML at all
            // is said once for both tables ([`say_not_toml`]); only a shape error is this
            // table's.
            if !say_not_toml(text) {
                let tail = "\u{2014} updates continue; new programs wait until it is fixed";
                crate::notice::say_once_with_log(
                    &format!("[packages] in aterm.toml: {e} {tail}"),
                    &format!("[packages] in aterm.toml: {} {tail}", e.log_words()),
                );
            }
            PackagesConfig::unreadable_table()
        }
    }
}

/// When `text` is not valid TOML, say so once for every table read from it and answer true: the parser fails the whole file, so a per-table line would blame a table that has nothing wrong with it.
fn say_not_toml(text: &str) -> bool {
    match not_toml_note(text) {
        Some((said, logged)) => {
            crate::notice::say_once_with_log(&said, &logged);
            true
        }
        None => false,
    }
}

/// The one line for an `aterm.toml` that is not valid TOML, or `None` when it parses: what a
/// person reads (the error with the line that did not parse) and what a host's log keeps (the
/// error's position, [`aterm_toml::Error::log_words`]).
fn not_toml_note(text: &str) -> Option<(String, String)> {
    let e = aterm_toml::from_str::<aterm_toml::Value>(text).err()?;
    let tail = "\u{2014} updates continue; new programs and the [machine] settings wait until \
                it is fixed";
    Some((
        format!("aterm.toml is not valid TOML ({e}) {tail}"),
        format!("aterm.toml is not valid TOML ({}) {tail}", e.log_words()),
    ))
}

/// The per-key admission a well-formed `[packages]` table gets, once, at load.
fn admit_packages(
    mut cfg: PackagesConfig,
    dev_seams: bool,
    home: Option<&Path>,
    report: bool,
) -> PackagesConfig {
    for (key, present) in [
        ("channel", cfg.channel.is_some()),
        ("include", cfg.include.is_some()),
    ] {
        if present && report {
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
    for note in cfg.admission_notes.iter().filter(|_| report) {
        crate::notice::say_spelling(note);
    }
    if report && let Some(ignored) = cfg.ignored_prefix.as_deref() {
        crate::notice::say_spelling_with_log(
            &ignored_prefix_note(ignored),
            &ignored_prefix_note_logged(ignored),
        );
    }
    cfg
}

/// What a person is told about a `[packages] prefix` this build ignores ([`PackagesConfig::ignored_prefix`]) — doctor's warn, Settings ▸ Packages' line and the load-time notice all say these words.
#[must_use]
pub fn ignored_prefix_note(prefix: &Path) -> String {
    ignored_prefix_words(&prefix.display())
}

/// [`ignored_prefix_note`] as a host's log keeps it: the path the person wrote by its length,
/// in the one template both forms fill.
fn ignored_prefix_note_logged(prefix: &Path) -> String {
    ignored_prefix_words(&aterm_log::words::Len(&prefix.display().to_string()))
}

/// The ignored-prefix notice's one template, filled with the prefix as a form shows it.
fn ignored_prefix_words(prefix: &dyn std::fmt::Display) -> String {
    format!(
        "[packages] prefix = {prefix} is not used by this build — aterm keeps its packages in \
         the one default store — so nothing is installed automatically until you remove \
         the line (what that prefix holds is left as it is)"
    )
}

/// Parse the `[machine]` table out of full `aterm.toml` text: no table ⇒ the defaults, which act.
#[must_use]
pub fn parse_machine(text: &str) -> MachineConfig {
    match aterm_toml::from_str::<MachineOnly>(text) {
        Ok(root) => root.machine.unwrap_or_default(),
        Err(e) => {
            // `inert()` makes every resolver leave the machine alone; `unreadable` lets the
            // CLI verdict, Settings' This Mac status and the `machine-state:` record say why
            // nothing is applied, rather than show a machine deliberately switched off.
            if !say_not_toml(text) {
                let tail = "\u{2014} its settings are not applied; Universal Control and Rosetta \
                            2 are left as they are";
                crate::notice::say_with_log(
                    &format!("[machine] in aterm.toml: {e} {tail}"),
                    &format!("[machine] in aterm.toml: {} {tail}", e.log_words()),
                );
            }
            MachineConfig {
                unreadable: true,
                ..MachineConfig::inert()
            }
        }
    }
}

/// Load the `[packages]` table from the real config file (absent or unreadable ⇒ defaults, malformed ⇒ the loud unreadable reading via [`parse_packages`]).
#[must_use]
pub fn load() -> PackagesConfig {
    let Some(path) = config_path() else {
        return PackagesConfig::default();
    };
    load_from_path(&path)
}

/// Read package settings for a read-only lookup, without notices or host-log writes.
/// A malformed or unreadable file refuses the lookup instead of silently selecting defaults.
pub(crate) fn load_for_lookup() -> Result<PackagesConfig, &'static str> {
    let Some(path) = config_path() else {
        return Ok(PackagesConfig::default());
    };
    let text = match crate::metadata_io::read_bounded_regular_utf8_follow_final_links(
        &path,
        MAX_PACKAGES_CONFIG_BYTES,
    ) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PackagesConfig::default());
        }
        Err(_) => return Err("package configuration is unreadable"),
    };
    let root: PackagesOnly =
        aterm_toml::from_str(&text).map_err(|_| "package configuration is malformed")?;
    Ok(admit_packages(
        root.packages.unwrap_or_default(),
        DEV_SEAMS,
        aterm_types::dirs::home_dir().as_deref(),
        false,
    ))
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
    crate::metadata_io::read_bounded_regular_utf8_follow_final_links(
        path,
        MAX_PACKAGES_CONFIG_BYTES,
    )
    .ok()
}

fn load_from_path(path: &Path) -> PackagesConfig {
    read_config_text(path).map_or_else(PackagesConfig::default, |t| parse_packages(&t))
}

/// The `[packages]` table of the config file at `path`, read now under [`load`]'s rules, for a long-lived reader that must see an edit (the window's package loop re-reads the Automatic-updates switch before every pass).
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

/// The process-wide `[packages]` config, read once per invocation (atpkg is a short-lived CLI).
#[must_use]
pub fn cached() -> &'static PackagesConfig {
    static CFG: std::sync::OnceLock<PackagesConfig> = std::sync::OnceLock::new();
    CFG.get_or_init(load)
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
