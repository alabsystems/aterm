// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The `[packages]` table of the SAME `aterm.toml` the GUI reads (§11) — atpkg's
//! own config reader.
//!
//! The GUI parses the whole file into its `Config` (including a mirror
//! `PackagesConfig` it uses only for the background-loop gate); atpkg reads just
//! the `[packages]` table out of the identical file, so there is exactly ONE
//! user-facing config surface — and NO environment alternative to it (2026-09-23, the
//! owner's R2: "the one true best batteries included default on path. Delete
//! alternatives. in the future, we could add settings, but NOT ENV VARS those are for
//! development"). `ATPKG_DISABLE`, `ATPKG_ACCOUNT`, `ATPKG_INDEX_REPO`, `ATPKG_TOKEN`
//! and `ATPKG_REFUSE_TRACKED_INSTALL` are gone; `ATPKG_REGISTRY` is a development
//! seam (`aterm_types::dev_seam!`).
//!
//! THE KEYS A PERSON HAS (Settings ▸ Packages writes the first two):
//!
//! * `enabled` — Automatic updates. Default TRUE. Off, no automatic lane runs (the
//!   window's loop, a terminal session's pass, the vendor head watch); a verb a person
//!   types still works. The retired `auto_update` is folded in: an `auto_update =
//!   false` still reads as off ([`PackagesConfig::enabled`]), and doctor names the
//!   rename once.
//! * `auto_install` — THE install consent, one key where there were two
//!   (`auto_install` for a network bootstrap, `seed_install` for the first-run fill).
//!   Default TRUE — batteries included: installing aterm is wanting the toolset, so the
//!   first run adopts and lays it down and later passes keep it COMPLETE as the signed
//!   set grows. [`PackagesConfig::auto_install`] has the exact rule and the migration.
//! * `exclude` — per-program opt-out, narrowing-only over the SIGNED index
//!   ([`crate::manifest::Index::installable`]).
//!
//! THE REST: `tracked_install` (machine policy for release-cutting machines, until
//! Phase 5 deletes its lane), and three DEVELOPMENT settings a shipped binary drops at
//! load with a notice ([`DEV_SEAMS`]): `prefix`, `account` and the `owner/repo` form of
//! `[packages.links]` (a checkout-path link is a dev link and stays). A dropped `prefix`
//! naming another store also stops unattended installs until the line is removed
//! ([`PackagesConfig::installs_unattended`]). Every load-time note is said ONCE per
//! process ([`crate::notice::say_spelling`]). `channel` and
//! `include` are retired: named once and ignored. Nothing here is a trust input: the
//! account is slug-validated downstream, and a `[packages.links]` entry can only
//! redirect WHERE bytes are fetched from or suppress registry management — never what
//! verifies (§5/§8: the host is not an authenticity input).
//!
//! A missing file, or a file without a `[packages]` table, yields the `[packages]`
//! defaults: automatic updates and installs on, no links, compiled account — the
//! posture of a machine that never wrote a config, which is most of them.
//!
//! A table we could not READ is NOT that case, and used to be treated as it.
//! CORRECTION 2026-09-16: this module claimed every `[packages]` default was the
//! inert/narrowest behavior, so a broken config "can only DISABLE bootstrap/links,
//! never widen anything". TWO defaults widen. The install consent (`auto_install`,
//! then spelled `seed_install`) defaults TRUE (§9.1), and on a LEAN install that lane is
//! a multi-GB DOWNLOAD; `exclude` defaults EMPTY, which excludes nothing. So the owner
//! who wrote `auto_install = false`, or `exclude = ["trust"]`, had precisely the two
//! keys that PREVENT large downloads reset to the permissive reading by a typo — a wrong TYPE in the table, or a TOML syntax
//! error ANYWHERE in the file, including the GUI-owned part this module does not
//! otherwise read. A malformed `[packages]` therefore falls to
//! [`PackagesConfig::unreadable_table`], loudly: the seed lane declines (so the machine
//! adopts nothing, and the network pass that follows adoption never fires), and the
//! unattended pass does not complete the set — the exclusions it would need are the
//! ones it could not read. What is already installed keeps updating, and an EXPLICIT
//! `install` still installs: fixing the file is one edit, and a machine frozen out of
//! security updates by a typo is a worse failure than the one this closes.
//!
//! `[machine]` is the ONE table that reasoning does not cover: its defaults ACT on the
//! host (a `com.apple.universalcontrol` write, a tree of renames under `$HOME`), so a
//! file we could not read falls to [`MachineConfig::inert`] instead — the same posture
//! [`MachineConfig::universal_control`] already takes for a single word it does not
//! recognise. Each table is also deserialized on its OWN ([`PackagesOnly`],
//! [`MachineOnly`]), so a typo can only cost the table it is written in.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Maximum `aterm.toml` size consumed by the co-located package verbs.
/// Matches the native config service's 512-KiB admission budget.
pub const MAX_PACKAGES_CONFIG_BYTES: usize = 512 * 1024;

/// The one channel whose pin set drives install/update. `[packages].channel` is retired
/// (2026-09-23): only `stable` has ever been published, so the key selected nothing but
/// a way to point a machine at a channel that does not exist.
pub const CHANNEL: &str = "stable";

/// Whether this build honours the DEVELOPMENT settings of `[packages]` — `prefix`,
/// `account` and the `owner/repo` form of `[packages.links]`: only a development build
/// does (`debug_assertions`, or this crate's `dev-seams` feature, which the release
/// cutter never enables). A shipped binary drops them at load and says so once
/// ([`admit_packages`]): one true path, the owner's rule.
pub const DEV_SEAMS: bool = cfg!(any(debug_assertions, feature = "dev-seams"));

/// The `[packages]` table. All-Option (an absent key and a default-valued key are
/// indistinguishable); defaults live ONLY in the resolver methods below.
#[derive(Debug, Default, Clone, serde::Deserialize)]
#[serde(default)]
pub struct PackagesConfig {
    /// `[packages].prefix`: the install prefix override. Absent ⇒
    /// [`crate::store::default_prefix`].
    ///
    /// CHAIN-VALIDATED, never trusted as written: [`crate::store::vet_prefix`] admits
    /// only a `$HOME` chain owned by our uid or a chain from `/` owned by root, each
    /// with no group/other-writable or symlinked component, and falls back to the
    /// default on any violation. So a hostile config can redirect the store only to a
    /// place that is already at least as safe as the default.
    ///
    /// CORRECTION 2026-08-18: this doc used to say a root-owned prefix was REQUIRED for
    /// the verified lane — that "Trust's verified launcher refuses a user-owned toolchain
    /// path", so `targo trust` could not run on anything atpkg installed under `$HOME`.
    /// That was true of an older Trust and is now false, and it is worth stating loudly
    /// because it nearly bought this project a `.pkg` installer, an admin prompt and a
    /// privileged updater daemon it does not need.
    ///
    /// Trust's default authority mode is `CallerOwned`
    /// (`targo/src/cargo/util/process_authority.rs`): a path component may be owned by
    /// root **or by the invoking identity**, and must not be group/world-writable. The
    /// DEFAULT `$HOME` prefix (0700, ours) satisfies it, so proving works with no
    /// privilege at all — Trust's own comment cites rustup's `~/.rustup` as the case that
    /// demanding root would break. Root is in fact REFUSED: targo bails when
    /// `effective_uid == 0`, because root-launched build scripts retain write authority
    /// over the execution objects.
    ///
    /// So the reason to set this is NOT the verified lane. It is a genuine system-wide
    /// install (one store shared by several users), or opting in to
    /// `TRUST_REQUIRE_SEALED_LAUNCHER=1`, whose sealed-release predicate Trust itself
    /// documents as still unimplementable. See `docs/GOLDEN-INSTALL-PATH.md` §2.
    ///
    /// A DEVELOPMENT setting since 2026-09-23 ([`DEV_SEAMS`]): a shipped binary drops it
    /// at load and uses the one default prefix — and while it names another store, installs
    /// nothing unattended ([`Self::ignored_prefix`], [`Self::installs_unattended`]).
    pub prefix: Option<String>,
    /// `[packages].enabled` — Automatic updates, THE switch (Settings ▸ Packages). Read
    /// through [`Self::enabled`], which folds in the retired `auto_update`. Gates the
    /// automatic lanes (the GUI's `spawn_pkg_update_check`, a terminal session's
    /// detached pass, the vendor head watch) and the unattended set completion; never a
    /// verb a person typed — an explicit `atpkg update` always works.
    pub enabled: Option<bool>,
    /// RETIRED 2026-09-23, folded into `enabled`: it gated the recurring loop alone while
    /// `enabled` gated the loop and the launch seed — two switches for one question. An
    /// existing `auto_update = false` is still honoured as `enabled = false`
    /// ([`Self::enabled`]) and doctor names the rename ([`Self::config_notes`]); Settings
    /// writes only `enabled` and drops this key when it does.
    pub auto_update: Option<bool>,
    /// `[packages].auto_install` — THE install consent (Settings ▸ Packages, "Install
    /// the ALab toolset"). Read through [`Self::auto_install`], which states the rule and
    /// the migration of the retired `seed_install`.
    pub auto_install: Option<bool>,
    /// RETIRED 2026-09-23, folded into `auto_install`: the first-run fill's consent, a
    /// second key for the one question "may atpkg install the toolset I did not name?".
    /// An existing `seed_install = false` is still honoured as `auto_install = false`
    /// when `auto_install` itself is unset ([`Self::auto_install`]), and doctor names
    /// the rename.
    pub seed_install: Option<bool>,
    /// Index owner override (e.g. `"alabsystems"`). Default = the compiled owner
    /// ([`crate::discovery::resolve_account`]). A DEVELOPMENT setting ([`DEV_SEAMS`]):
    /// a shipped binary drops it at load. Slug-validated downstream — a malformed value
    /// can never redirect fetches.
    pub account: Option<String>,
    /// RETIRED 2026-09-23 ([`CHANNEL`]): parsed as any value so a stale key never fails
    /// the table, named once at load, never read.
    pub channel: Option<aterm_toml::Value>,
    /// RETIRED 2026-09-23: the narrowing include filter. A machine keeps the whole signed
    /// set minus `exclude`; parsed as any value, named once at load, never read.
    pub include: Option<aterm_toml::Value>,
    /// Narrowing-only exclude filter: the per-program opt-out.
    pub exclude: Option<Vec<String>>,
    /// `[packages].tracked_install`: what a provenance-tracked pass does with files whose
    /// macOS tag could not be cleared ([`crate::lay`], "When the lane cannot run") —
    /// `"record"` keeps them and, for a staged bundle, records it beside the build (the
    /// default since 2026-09-14); `"refuse"` fails the install instead. The tag is cleared
    /// in place first either way ([`crate::provenance::heal`]), so on a working Mac
    /// neither word changes anything. Default `record`.
    ///
    /// WHY A CONFIG KEY (2026-09-15): the knob was env-only
    /// (`ATPKG_REFUSE_TRACKED_INSTALL`), and the window's own update/seed passes are
    /// spawned from launchd's environment, which no shell export reaches — so a
    /// release-cutting machine that wanted the refusal could have it for a pass typed
    /// by hand and never for the unattended passes that lay most of its toolchain. This
    /// key is the ONE spelling now: the env var is gone (2026-09-23), so no shell can
    /// give one run a policy the machine's passes do not have.
    /// Admitted at load ([`parse_packages`]): any word but the two is named once (as an
    /// unasked notice, [`crate::notice`]) and treated as unset, so a typo falls to
    /// `record` — the reading that does not intervene — and never takes the rest of the
    /// table down with it.
    pub tracked_install: Option<String>,
    /// `[packages.links]`: `name = "/path/to/checkout"` (or `~/…`) declares a
    /// managed dev-link ([`crate::linkmode`] — registry management skipped; a
    /// maintainer's key, never in Settings); `name = "owner/repo"` declares a
    /// private-repo FETCH override for that program's release assets (signature
    /// verification UNCHANGED) — a DEVELOPMENT setting ([`DEV_SEAMS`]) a shipped
    /// binary drops at load. Anything else is refused loudly ([`LinkTarget::Invalid`]).
    pub links: BTreeMap<String, String>,
    /// NOT a config key — `#[serde(skip)]`, so no `aterm.toml` can set it. True only of
    /// the table [`parse_packages`] could not read
    /// ([`PackagesConfig::unreadable_table`]). It is a bare `bool` in an otherwise
    /// all-`Option` struct because it is not a key: an absent key and a default-valued
    /// key are indistinguishable here on purpose, and this is neither. Every resolver
    /// whose default WIDENS consults it (see the module doc).
    #[serde(skip)]
    pub unreadable: bool,
    /// NOT a config key — `#[serde(skip)]`. The `[packages] prefix` a SHIPPED binary
    /// dropped at load ([`admit_packages`]) because it names a store other than the one
    /// default, resolved against the home it was read under. While it is set, nothing is
    /// installed unattended ([`Self::installs_unattended`]): a machine that kept its
    /// toolset somewhere else is not silently given a second, multi-GB copy in the default
    /// prefix, with the shell hook and the rustup link re-pointed at it. Removing the key
    /// is the whole fix; doctor and Settings ▸ Packages say so.
    #[serde(skip)]
    pub ignored_prefix: Option<PathBuf>,
    /// NOT a config key — `#[serde(skip)]`. What [`admit_packages`] took out of the table,
    /// in its own words: a development setting a shipped binary does not read, or a
    /// `tracked_install` word that is neither spelling. Said once at load and repeated by
    /// doctor ([`Self::config_notes`]), which is where a person looks.
    #[serde(skip)]
    pub admission_notes: Vec<String>,
}

impl PackagesConfig {
    /// The reading of a `[packages]` table that FAILED to parse — the twin of
    /// [`MachineConfig::inert`], and for the same reason: not every default here is
    /// inert, and a file we could not read is no licence to act on the owner's behalf.
    /// No key is invented into the table (the owner wrote none of them); the flag is
    /// what makes the widening resolvers fail closed.
    #[must_use]
    pub fn unreadable_table() -> Self {
        Self {
            unreadable: true,
            ..Self::default()
        }
    }
    /// Automatic updates — `[packages].enabled`, default TRUE, with the retired
    /// `auto_update` folded in: an `auto_update = false` still left in a file reads as
    /// off (its "the loop is off" meant exactly that), whatever `enabled` says, until
    /// the file is rewritten — Settings drops the old key when it writes `enabled`.
    /// A table we could not read is ON: what is installed keeps updating (the module
    /// docs' rule — a typo must not freeze a machine out of security updates).
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

    /// THE INSTALL CONSENT — whether atpkg may install toolset members nobody named:
    /// the first-run `seed` pass adopts the machine and lays the set down, and every
    /// update pass after it installs what the signed default set gained. Default TRUE,
    /// batteries included (§9.1): installing aterm is wanting the toolset. The set is a
    /// multi-GB download (no current client reads a sealed seed — Phase 5 deleted that
    /// lane), and this key is the one switch that prevents it — so every user-facing
    /// description of it says so (audit-2 item 7).
    ///
    /// The explicit opt-outs ALWAYS win over it, whatever it says: `uninstall <p>`
    /// (the program's removed marker), `uninstall --all` (the decline), `exclude`, and
    /// `enabled = false` (no unattended pass runs to install anything). A verb a person
    /// types (`install <p>`, `install --default-set`) is its own consent and ignores
    /// this key.
    ///
    /// MIGRATION of the two keys this replaced (2026-09-23): `auto_install` itself wins
    /// when written; else the retired `seed_install` (its `false` was the documented
    /// "not on my disk"); else TRUE. So `seed_install = false` still keeps a machine
    /// bare, `auto_install = true` still installs, and an `auto_install = false` —
    /// before today a no-op spelling of the old default — now means what it says: no
    /// unattended installs (doctor names both, [`Self::config_notes`]).
    ///
    /// A table we could not READ resolves FALSE, whatever the file said: the default is
    /// the one that costs gigabytes, and `auto_install = false` is the documented way to
    /// say "not on my disk" — a typo elsewhere in `aterm.toml` must not repeal it
    /// ([`PackagesConfig::unreadable_table`]).
    #[must_use]
    pub fn auto_install(&self) -> bool {
        !self.unreadable && self.auto_install.or(self.seed_install).unwrap_or(true)
    }

    /// Whether atpkg may install anything nobody named, HERE AND NOW: the install consent
    /// ([`Self::auto_install`]) AND no configured prefix being ignored
    /// ([`Self::ignored_prefix`]). The first-run seed and the update pass's set completion
    /// read this; Settings shows the consent itself. A shipped binary keeps ONE store, the
    /// default (2026-09-23), so a `[packages] prefix` naming another one is dropped — and
    /// until its owner removes the line, the machine is treated as not wanting the toolset
    /// in the default store: the toolset they have lives where the key points, and filling
    /// a second store beside it (gigabytes, over the network on a lean install) and moving
    /// the shell hook and the rustup `trust` link onto it is not what that line asked for.
    #[must_use]
    pub fn installs_unattended(&self) -> bool {
        self.auto_install() && self.ignored_prefix.is_none()
    }

    /// What doctor says about this table's spelling — one line per key a person should
    /// rename or remove: the retired `auto_update`/`seed_install` (still honoured), and
    /// the retired `channel`/`include` (ignored). The development settings a shipped
    /// binary dropped are named at load ([`admit_packages`]), where they are dropped.
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

    /// What `[packages].tracked_install` selects — `Some(Refuse)` for `"refuse"`,
    /// `Some(Allow)` for `"record"` — or `None` when the key is absent, blank, or not one
    /// of the two spellings (which [`parse_packages`] already named, [`crate::notice`], and
    /// dropped; a table built by hand and never parsed gets the same `None`, silently).
    /// The default is [`crate::lay::tracked_policy_of`]'s, not this method's — one place
    /// decides.
    #[must_use]
    pub fn tracked_install(&self) -> Option<crate::lay::TrackedPolicy> {
        tracked_install_of(self.tracked_install.as_deref()?)
    }
}

/// The line a retired `auto_update` gets, for doctor and (the same words) the config
/// editor. With no `enabled` beside it the fix is the rename. With `enabled` ALREADY in the
/// table, a rename would write the key twice — a file that no longer parses, which atpkg
/// reads as automatic updates ON — so the fix is to delete the old key, and, when the old
/// key is what keeps updates off, to say that off in `enabled`.
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

/// The two spellings of `[packages].tracked_install`, trimmed and case-sensitive (like
/// `[machine] universal_control`: `"REFUSE"` is a word the owner did not write).
fn tracked_install_of(value: &str) -> Option<crate::lay::TrackedPolicy> {
    match value.trim() {
        "record" => Some(crate::lay::TrackedPolicy::Allow),
        "refuse" => Some(crate::lay::TrackedPolicy::Refuse),
        _ => None,
    }
}

/// What one `[packages.links]` value means. Classified fail-closed: only the two
/// sanctioned shapes act; everything else is [`LinkTarget::Invalid`] and ignored
/// loudly at the consumption site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// A local checkout path (absolute, or `~`-anchored) → managed dev-link.
    Checkout(PathBuf),
    /// A slug-validated `owner/repo` → private-repo fetch override for the
    /// program's release assets (the SIGNED index must still name the program —
    /// reachability §5 is untouched, so this can never install from a bare slug).
    Repo(String),
    /// Neither shape (relative path, `~user`, URL-metacharacter slug, …).
    Invalid,
}

impl PackagesConfig {
    /// The configured install prefix as an absolute path, or `None` for the default.
    ///
    /// `~`/`~/…` expands against `home` (same rule as `[packages.links]`: no `~user`).
    /// A relative path resolves to `None` rather than being joined onto anything — a
    /// prefix that depends on the process CWD is not a prefix. This performs NO trust
    /// check; [`crate::store::vet_prefix`] is the sole authority on whether the result
    /// is safe, and it fails closed to the default.
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

/// Classify one `[packages.links]` value. `home` backs `~` expansion (injected
/// for testability; `None` means a `~` value cannot resolve and is Invalid).
/// A repo value must be exactly `owner/repo` with BOTH parts passing the shared
/// URL-safety allowlist ([`aterm_update_core::is_valid_slug`]) so a malformed
/// value can never redirect a fetch off the GitHub API.
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

/// The program → `owner/repo` FETCH-override map derived from `[packages.links]`
/// (the [`LinkTarget::Repo`] entries only), consumed by
/// [`crate::GithubFetcher::with_overrides`]. Checkout/Invalid entries are the
/// link-reconciliation path's business, not the fetcher's.
#[must_use]
pub fn repo_overrides(cfg: &PackagesConfig) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (program, value) in &cfg.links {
        // `home: None` is fine here: a `~…` value classifies as Checkout/Invalid
        // either way, never as Repo, and only Repo entries matter for fetches.
        if let LinkTarget::Repo(slug) = classify_link(value, None) {
            out.insert(program.clone(), slug);
        }
    }
    out
}

/// The path of the user config file — EXACTLY the GUI's resolution
/// (`app_config.rs::config_path`, mirrored so the two can never read different
/// files): `$XDG_CONFIG_HOME/aterm/aterm.toml`, else (Windows)
/// `%APPDATA%\aterm\aterm.toml`, else `$HOME/.config/aterm/aterm.toml`.
#[must_use]
pub fn config_path() -> Option<PathBuf> {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|x| !x.is_empty()) {
        return Some(PathBuf::from(x).join("aterm").join("aterm.toml"));
    }
    #[cfg(windows)]
    if let Some(appdata) = std::env::var_os("APPDATA").filter(|a| !a.is_empty()) {
        return Some(PathBuf::from(appdata).join("aterm").join("aterm.toml"));
    }
    // `HOME` first, the ACCOUNT's home second. A process launched without `HOME` (a
    // launchd job, `su` without `-`, a sandbox) used to resolve NO config file — which
    // for this table is not "no settings" but "both defaults act", so an explicit
    // `universal_control = "leave"` was silently replaced by the acting default and the
    // pass wrote the real account's per-host keys against the owner's stated wish.
    // `defaults` follows the account regardless of `$HOME`, so reading the account's
    // config in that state is the only reading that matches what would be written.
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|h| !h.as_os_str().is_empty())
        .or_else(crate::platform::account_home)
        .map(|h| h.join(".config/aterm/aterm.toml"))
}

/// The `[machine]` table — the machine settings `aterm pkg machine apply` applies at
/// first open and keeps applied (R5, owner decisions 2026-09-10; a pass carries only an
/// edit to the table since Phase 3). Read by atpkg because atpkg
/// APPLIES them: the GUI only renders the resulting `machine-settings:` row.
///
/// All-Option like [`PackagesConfig`]; defaults live ONLY in the resolver methods. Both
/// defaults ACT — this is the one table whose "absent" means "do the doctor's remedy" —
/// so the opt-outs are spelled explicitly: `spotlight_noindex = false`,
/// `universal_control = "leave"`.
#[derive(Debug, Default, Clone, serde::Deserialize)]
#[serde(default)]
pub struct MachineConfig {
    /// `[machine].spotlight_noindex`: rename every cargo target dir the doctor's scan
    /// finds under `$HOME` to its `.noindex` form and keep that repo's cargo pointed at
    /// it (a `target` symlink in a git checkout, a `.cargo/config.toml` edit elsewhere)
    /// — by `aterm pkg machine apply` (which the window runs as it opens and a terminal
    /// session once a day), and at a seed/update/install pass when the `[machine]` table
    /// changed.
    /// Default `true`.
    pub spotlight_noindex: Option<bool>,
    /// `[machine].universal_control`: `"off"` (default) writes
    /// `com.apple.universalcontrol Disable`/`DisableMagicEdges` for the current host when
    /// they are not already set; `"leave"` never touches them.
    pub universal_control: Option<String>,
    /// The config file EXISTS and could not be parsed, so neither opt-out above could be
    /// read. Never deserialized — [`parse_machine`] sets it.
    ///
    /// FAIL CLOSED (2026-09-15). Both defaults ACT, so "malformed ⇒ defaults" meant a
    /// single typo anywhere in `aterm.toml` — a broken `[keys]` binding three tables
    /// away — silently re-enabled the very changes the owner had switched off with
    /// `universal_control = "leave"`, and said so only in one stderr line nothing
    /// surfaces. A table whose defaults change the machine may not be inferred from a
    /// file nobody could read.
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
    /// The reading a `[machine]` table we could not PARSE gets: touch nothing, with both
    /// opt-outs spelled out.
    ///
    /// Not [`Self::default`], which is the opposite — every default in this table ACTS,
    /// so falling to it over an unreadable file meant a hand-edited typo silently
    /// re-enabled the very writes the owner had opted out of. Acting on words we could
    /// not read is exactly what [`Self::universal_control`] refuses to do for ONE
    /// unrecognised spelling; a whole table we cannot read deserves no more licence.
    #[must_use]
    pub fn inert() -> Self {
        Self {
            spotlight_noindex: Some(false),
            universal_control: Some("leave".to_string()),
            // An inert table is not by itself an unreadable one: a caller may build
            // this for its own reasons. `parse_machine` sets the flag when the file is
            // what could not be read.
            unreadable: false,
        }
    }

    /// `[machine].spotlight_noindex`, default `true`.
    #[must_use]
    pub fn spotlight_noindex(&self) -> bool {
        self.spotlight_noindex.unwrap_or(true)
    }

    /// `[machine].universal_control`, default `off`. An unrecognised spelling is
    /// `Leave` — the inert reading — and says so once ([`crate::notice`]), rather than
    /// acting on a machine setting over a word the owner did not write.
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

/// Deserialization wrapper for the `[packages]` table ALONE; every other table/key of
/// the whole `aterm.toml` is ignored (the GUI owns that schema).
///
/// ONE TABLE PER WRAPPER, and that is the point. A single `RootConfig { packages,
/// machine }` type-checked BOTH tables in one deserialization, so an `auto_install =
/// "yes"` typo in `[packages]` failed the whole parse and handed [`parse_machine`] a
/// `MachineConfig::default()` — whose defaults ACT — throwing away an explicit
/// `[machine] universal_control = "leave"` and letting the next pass disable Universal
/// Control on a machine whose owner had opted out. A table a wrapper does not NAME is
/// skipped without being type-checked, so a typo now costs only the table it is in.
#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct PackagesOnly {
    packages: Option<PackagesConfig>,
}

/// Deserialization wrapper for the `[machine]` table alone — the twin of
/// [`PackagesOnly`], and read only by [`parse_machine`].
#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct MachineOnly {
    machine: Option<MachineConfig>,
}

/// The `[reroute]` table — what a session does with the upstream Rust names
/// ([`crate::reroute`]). ONE key, and it is the owner's 2026-09-08 ask ("some kind of
/// printed message when using these tools that could be suppressed with a flag") made
/// a SETTING on 2026-09-23 ("NOT ENV VARS those are for development"): it replaces
/// `ATERM_REROUTE_QUIET`. Settings ▸ Packages writes it.
#[derive(Debug, Default, Clone, serde::Deserialize)]
#[serde(default)]
pub struct RerouteConfig {
    /// `[reroute].announce`: whether a SIGNPOST row (`cargo`, `rustc`, `tlc`) prints its
    /// announcement before it runs the upstream tool. Default TRUE. `false` silences the
    /// line and changes nothing else — the upstream tool still runs, a DIRECT row still
    /// says what it substituted (that line is the "never silently substituting"
    /// guarantee itself), and the ORACLE row still refuses.
    pub announce: Option<bool>,
}

impl RerouteConfig {
    /// `[reroute].announce`, default TRUE.
    #[must_use]
    pub fn announce(&self) -> bool {
        self.announce.unwrap_or(true)
    }
}

/// Deserialization wrapper for the `[reroute]` table alone — the one-table discipline of
/// [`PackagesOnly`].
#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct RerouteOnly {
    reroute: Option<RerouteConfig>,
}

/// Parse the `[reroute]` table out of full `aterm.toml` text: no table, or one that does
/// not parse, is the default — ANNOUNCE, the reading that says more, never less (a
/// typo must not silently hide which toolchain a build ran on).
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
    CFG.get_or_init(|| {
        config_path()
            .and_then(|p| read_config_text(&p))
            .map_or_else(RerouteConfig::default, |t| parse_reroute(&t))
    })
}

/// Parse the `[packages]` table out of full `aterm.toml` text. A file without the
/// table ⇒ defaults; a malformed file ⇒ the loud UNREADABLE reading
/// ([`PackagesConfig::unreadable_table`] — never aborts a verb, and never the plain
/// defaults, two of which WIDEN: see the module doc). A well-formed table then passes
/// [`admit_packages`], the per-key admission.
#[must_use]
pub fn parse_packages(text: &str) -> PackagesConfig {
    parse_packages_with(text, DEV_SEAMS)
}

/// [`parse_packages`] with the build's seam posture as an input, so a test build pins
/// what a shipped binary makes of the development settings. `home` is the one the
/// process resolves ([`aterm_types::dirs::home_dir`]): a dropped `prefix` is IGNORED only
/// when it names a store other than the default under it ([`admit_packages`]).
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
            // Said wherever this process's unasked notices go ([`crate::notice`]): a typed
            // verb's stderr, or the host's log — a session launch reads this table too.
            // ONCE per process: every layout resolution reads the file again.
            crate::notice::say_once(&format!(
                "aterm.toml has an error in [packages] \u{2014} updates continue, but new \
                 programs are not installed until it is fixed: {e}"
            ));
            PackagesConfig::unreadable_table()
        }
    }
}

/// The per-key admission a WELL-FORMED `[packages]` table still gets, once, at load.
///
/// RETIRED AND DEVELOPMENT KEYS (2026-09-23). A retired `channel`/`include` is named
/// ([`crate::notice::say_spelling`]) and never read. In a shipped binary (`dev_seams`
/// false) the development settings — `prefix`, `account`, and every `owner/repo` value of
/// `[packages.links]` — are named and DROPPED from the table, so no resolver downstream
/// can act on them: one true path. A checkout-path link stays (a maintainer's dev link,
/// never in Settings). A dropped `prefix` that names a store other than the default under
/// `home` is also kept as [`PackagesConfig::ignored_prefix`], which stops unattended
/// installs until the line is removed ([`PackagesConfig::installs_unattended`]).
///
/// EVERY NOTE HERE IS SAID ONCE PER PROCESS, and never by one that reports the spelling
/// itself (doctor) or says nothing unasked (`__reroute`) — [`crate::notice`]. This table is
/// read by every layout resolution, and a note said per read was said three times by one
/// `doctor` and before every `cargo` a session ran. What was dropped is also kept in
/// [`PackagesConfig::admission_notes`], which doctor prints.
///
/// And the tracked-install word: a
/// `tracked_install` that is neither `"record"` nor `"refuse"` (a blank is merely unset)
/// is named and dropped from the table, so no resolver ever sees a word the owner did not
/// write. It is the loud-defaults posture of a malformed file narrowed to the one key — a
/// typo here must not zero `exclude` — and, like `[machine] universal_control`'s unknown
/// word, it falls to the reading that does NOT intervene: `record`, the default, never
/// `refuse`. The owner's 2026-09-14 ruling is that a lane that cannot run must not leave a
/// machine with no toolchain unless the refusal was spelled exactly; the doctor and the
/// release cutter's pre-claim gate still name a tagged toolchain either way.
fn admit_packages(mut cfg: PackagesConfig, dev_seams: bool, home: Option<&Path>) -> PackagesConfig {
    if let Some(raw) = cfg.tracked_install.as_deref()
        && !raw.trim().is_empty()
        && tracked_install_of(raw).is_none()
    {
        cfg.admission_notes.push(format!(
            "[packages] tracked_install must be \"record\" or \"refuse\", not {raw:?} — \
             using \"record\""
        ));
        cfg.tracked_install = None;
    }
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
        if cfg.prefix.take().is_some_and(|p| !p.trim().is_empty()) {
            dropped.push("prefix".to_string());
        }
        if let (Some(configured), Some(home)) = (configured, home)
            && configured != crate::store::default_prefix(home)
        {
            cfg.ignored_prefix = Some(configured);
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
    // Not a `note`: doctor WARNS it and Settings ▸ Packages shows it, because it stops
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
/// which ACT (the doctor's remedy is what an absent table asks for).
///
/// A table we cannot READ is the one place this parts company with [`parse_packages`]:
/// it yields the loud INERT reading ([`MachineConfig::inert`]), never the acting
/// defaults. `[packages]` is type-checked by its own wrapper, so a typo over there can
/// no longer decide this machine's settings either.
#[must_use]
pub fn parse_machine(text: &str) -> MachineConfig {
    match aterm_toml::from_str::<MachineOnly>(text) {
        Ok(root) => root.machine.unwrap_or_default(),
        Err(e) => {
            // BOTH HALVES, and they answer different questions. Upstream's `inert()`
            // makes the RESOLVERS safe — every reader of this table gets "leave it
            // alone" over a file nobody could read — and `unreadable` makes the
            // surfaces HONEST: the CLI verdict, the "This Mac" card and the
            // `machine-state:` record can say WHY nothing is being applied, instead of
            // rendering a machine that looks deliberately switched off.
            crate::notice::say(&format!(
                "malformed aterm.toml — the [machine] settings are not applied, and \
                 Universal Control and Spotlight are left exactly as they are: {e}"
            ));
            MachineConfig {
                unreadable: true,
                ..MachineConfig::inert()
            }
        }
    }
}

/// Load the `[packages]` table from the real config file (missing/unreadable ⇒
/// defaults, malformed ⇒ loud defaults via [`parse_packages`]).
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
    // The config file itself may legitimately be a dotfile-manager symlink.
    // Resolve only that bounded final-link chain, then admit/read the selected
    // regular target through one non-blocking, bounded handle.
    crate::metadata_io::read_bounded_regular_utf8_follow_final_links(
        path,
        MAX_PACKAGES_CONFIG_BYTES,
    )
    .ok()
}

fn load_from_path(path: &Path) -> PackagesConfig {
    // not present / unreadable → defaults
    read_config_text(path).map_or_else(PackagesConfig::default, |t| parse_packages(&t))
}

/// The `[packages]` table of the config file at `path`, read NOW under [`load`]'s rules —
/// for a long-lived reader that must see an edit (the window's package loop re-reads the
/// Automatic-updates switch before every pass, Phase 4). `None` when the file exists but
/// cannot be read: a transient failure must not read as the defaults.
#[must_use]
pub fn load_live(path: &Path) -> Option<PackagesConfig> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(PackagesConfig::default()),
        Err(_) => None,
        Ok(_) => read_config_text(path).map(|t| parse_packages(&t)),
    }
}

/// The process-wide `[machine]` config, read ONCE per invocation like [`cached`].
#[must_use]
pub fn cached_machine() -> &'static MachineConfig {
    static CFG: std::sync::OnceLock<MachineConfig> = std::sync::OnceLock::new();
    CFG.get_or_init(load_machine)
}

/// The process-wide `[packages]` config, read ONCE per invocation (atpkg is a
/// short-lived CLI; there is no reload seam to keep coherent). There is no
/// environment alternative to any of it (module docs).
#[must_use]
pub fn cached() -> &'static PackagesConfig {
    static CFG: std::sync::OnceLock<PackagesConfig> = std::sync::OnceLock::new();
    CFG.get_or_init(load)
}

#[cfg(test)]
mod tests {
    use super::*;

    // `[machine]`: both defaults ACT (spotlight on, Universal Control off); the two
    // opt-outs are the explicit spellings; a word the owner did not write is inert.
    #[test]
    fn machine_table_defaults_act_and_opt_outs_are_explicit() {
        let none = parse_machine("font_px = 12.0\n[packages]\nexclude = [\"trust\"]\n");
        assert!(none.spotlight_noindex());
        assert_eq!(none.universal_control(), UniversalControlPolicy::Off);
        let off =
            parse_machine("[machine]\nspotlight_noindex = false\nuniversal_control = \"leave\"\n");
        assert!(!off.spotlight_noindex());
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
        assert!(!unreadable.spotlight_noindex());
        // The two tables come out of one file, independently.
        let both = "[packages]\nexclude = [\"ay\"]\n[machine]\nspotlight_noindex = false\n";
        assert_eq!(parse_packages(both).exclude(), ["ay".to_string()]);
        assert!(!parse_machine(both).spotlight_noindex());
    }

    /// A TYPO IN `[packages]` MUST NOT RE-ARM THE ACTING `[machine]` DEFAULTS.
    /// Both tables used to come out of ONE deserialization of the whole file, so a
    /// type error in `[packages]` — a table [`parse_machine`] does not even read —
    /// threw away the owner's explicit `[machine]` opt-outs and handed the pass the
    /// defaults, which ACT: the next seed/update/install pass wrote
    /// `com.apple.universalcontrol Disable`/`DisableMagicEdges` and renamed cargo
    /// target dirs on a machine whose owner had written `universal_control = "leave"`
    /// and `spotlight_noindex = false`, and the only notice was one `eprintln!` from a
    /// background child whose stderr nobody reads. Each table is parsed on its OWN
    /// now, and a `[machine]` table that cannot be read falls to the INERT reading —
    /// never to the acting one.
    #[test]
    fn a_packages_typo_never_re_arms_the_machine_defaults() {
        let typo = "[packages]\nauto_install = \"yes\"\n[machine]\n\
                    universal_control = \"leave\"\nspotlight_noindex = false\n";
        let m = parse_machine(typo);
        assert_eq!(
            m.universal_control(),
            UniversalControlPolicy::Leave,
            "a [packages] type error must not cancel an explicit universal_control opt-out"
        );
        assert!(
            !m.spotlight_noindex(),
            "a [packages] type error must not cancel an explicit spotlight_noindex opt-out"
        );
        // The mirror: a `[machine]` type error must not silence the `[packages]` table.
        let other = "[packages]\nexclude = [\"ay\"]\n[machine]\nspotlight_noindex = 3\n";
        assert_eq!(
            parse_packages(other).exclude(),
            ["ay".to_string()],
            "a [machine] type error must not discard the [packages] table"
        );
        // A file that is not TOML at all: nothing readable said to act, so nothing acts.
        let broken = parse_machine("[machine\nnot toml");
        assert_eq!(broken.universal_control(), UniversalControlPolicy::Leave);
        assert!(!broken.spotlight_noindex());
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
        assert_eq!(
            cfg.tracked_install(),
            None,
            "tracked_install absent is unset — the default (record) is lay's to apply"
        );
        assert_eq!(cfg.enabled, None);
        assert_eq!(cfg.auto_update, None);
    }

    #[test]
    fn full_table_parses_every_key() {
        let cfg = parse_packages_with(
            "[packages]\nenabled = true\nauto_install = false\n\
             account = \"alabsystems\"\nexclude = [\"trust\"]\n\
             tracked_install = \"refuse\"\n\
             [packages.links]\nay = \"~/ay\"\norc = \"alabsystems/orc\"\n",
            true,
        );
        assert_eq!(cfg.enabled, Some(true));
        assert!(cfg.enabled());
        assert!(!cfg.auto_install());
        assert_eq!(cfg.account(), Some("alabsystems"));
        assert_eq!(cfg.exclude(), ["trust".to_string()]);
        assert_eq!(
            cfg.tracked_install(),
            Some(crate::lay::TrackedPolicy::Refuse)
        );
        assert_eq!(cfg.links.get("ay").map(String::as_str), Some("~/ay"));
        assert_eq!(
            cfg.links.get("orc").map(String::as_str),
            Some("alabsystems/orc")
        );
    }

    /// `auto_update` is folded into `enabled` (2026-09-23): its `false` still reads as
    /// off, whatever `enabled` says, and doctor names the rename; `true` changes nothing.
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

    /// THE RENAME IS SAID ONLY WHERE A RENAME PARSES. With `enabled` already written,
    /// "rename it to `enabled`" produced `enabled = true` then `enabled = false` — a
    /// duplicate key, a file that no longer parses, and atpkg reading automatic updates
    /// ON: the owner's opt-out reversed by following the advice. Measured before this fix:
    /// the both-keys table got the rename line.
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

    /// A PREFIX THIS BUILD IGNORES STOPS UNATTENDED INSTALLS (2026-09-23). A shipped
    /// binary keeps one store; a `[packages] prefix` naming another is dropped, and until
    /// the line goes the seed and the set completion install nothing into the default
    /// store — the toolset lives where the key points. One naming the default itself, and
    /// every development build, change nothing. Doctor and Settings name it.
    #[test]
    fn an_ignored_prefix_stops_unattended_installs_until_the_line_is_removed() {
        let home = Path::new("/Users//someone");
        let shared = parse_packages_at(
            "[packages]\nprefix = \"/opt/aterm/pkg\"\n",
            false,
            Some(home),
        );
        assert_eq!(shared.prefix, None, "dropped");
        assert_eq!(
            shared.ignored_prefix.as_deref(),
            Some(Path::new("/opt/aterm/pkg"))
        );
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
            shared
                .config_notes()
                .iter()
                .any(|n| n.contains("[packages] prefix")),
            "the drop itself is a note: {:?}",
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
            "[packages]\nprefix = \"/opt/aterm/pkg\"\n",
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

    /// ONE install consent (2026-09-23): `auto_install` wins when written, else the
    /// retired `seed_install`, else ON — and a table we could not read is OFF.
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
        assert_eq!(CHANNEL, "stable");
    }

    /// THE DEVELOPMENT SETTINGS (2026-09-23): a shipped binary drops `prefix`, `account`
    /// and every `owner/repo` link at load, so no resolver can act on them; a checkout
    /// link stays. A development build keeps them all.
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

    // Malformed config ⇒ the loud UNREADABLE reading, never a panic/abort.
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
        // A [packages] table of the WRONG SHAPE (scalar) also fails to defaults.
        let cfg = parse_packages("packages = 3\n");
        assert!(cfg.exclude().is_empty());
        assert!(cfg.unreadable);
    }

    /// A `[packages]` table we could not read must not WIDEN — the one claim this
    /// module made about malformed files that was FALSE (2026-09-16 audit).
    ///
    /// The install consent (then `seed_install`, now `auto_install`) defaults TRUE and
    /// `exclude` defaults EMPTY, so resetting the table
    /// to its defaults switched ON the multi-GB paths those two keys exist to block: the
    /// owner of a lean install (an Intel Mac, a Linux box, any cut since 2026-08-26 —
    /// none seal a seed) who wrote `seed_install = false` got the seed lane installing
    /// anyway on the next launch, recording adoption, after which `cmd_update_all` sees
    /// `complete_the_set` and pulls the whole set over the network. The trigger needs no mistake in `[packages]` at all: the GUI owns
    /// most of `aterm.toml`, and a syntax error anywhere in it fails this parse too.
    #[test]
    fn an_unreadable_packages_table_never_widens_the_download_gates() {
        // A wrong TYPE inside [packages] — the whole table fails, as
        // `tracked_install = true` already showed — with auto_install = false written
        // right above it.
        let typed = parse_packages("[packages]\nauto_install = false\nenabled = \"yes\"\n");
        assert!(typed.unreadable);
        assert!(
            !typed.auto_install(),
            "a table we could not read is not consent to lay the toolset down: the \
             resolver's TRUE default is the expensive one, and on a lean install it is a \
             multi-GB download the owner declined in writing"
        );
        // A TOML syntax error in the GUI-OWNED part of the same file: [packages] itself
        // is spotless and still unreadable, because the document never parses.
        let elsewhere = parse_packages("[packages]\nauto_install = false\n[machine\nbroken\n");
        assert!(elsewhere.unreadable);
        assert!(!elsewhere.auto_install());
        // And exclude, the other widening default: it cannot be recovered, so the flag
        // is what the set-completion lane reads instead (`cli::should_complete_set`).
        let excluded = parse_packages("[packages]\nexclude = [\"trust\"]\nenabled = 7\n");
        assert!(excluded.unreadable);
        assert!(
            excluded.exclude().is_empty(),
            "the exclusions are genuinely gone — which is exactly why the pass that \
             would act on their absence must not run"
        );
        // A READABLE table is untouched by any of this: the default stays TRUE (§9.1).
        let fine = parse_packages("[packages]\nexclude = []\n");
        assert!(!fine.unreadable);
        assert!(fine.auto_install());
        // A hand-built table (never parsed) is readable by construction.
        assert!(!PackagesConfig::default().unreadable);
        assert!(PackagesConfig::default().auto_install());
        assert!(PackagesConfig::unreadable_table().unreadable);
        assert!(!PackagesConfig::unreadable_table().auto_install());
    }

    /// `[packages].tracked_install` (2026-09-15): the two spellings resolve; absent and
    /// blank are unset (the `record` default is `lay::tracked_policy_of`'s); a word the owner did not write is dropped AT LOAD —
    /// to unset, never to `refuse` — and the rest of the table survives it; a value of
    /// the wrong TYPE is the whole table's malformed-file posture (loud defaults). Never
    /// a panic in any of these.
    #[test]
    fn tracked_install_admits_two_spellings_and_drops_the_rest_at_load() {
        use crate::lay::TrackedPolicy;
        let of = |toml: &str| parse_packages(toml).tracked_install();
        assert_eq!(of(""), None);
        assert_eq!(of("[packages]\nexclude = []\n"), None);
        assert_eq!(
            of("[packages]\ntracked_install = \"record\"\n"),
            Some(TrackedPolicy::Allow)
        );
        assert_eq!(
            of("[packages]\ntracked_install = \"refuse\"\n"),
            Some(TrackedPolicy::Refuse)
        );
        assert_eq!(
            of("[packages]\ntracked_install = \" refuse \"\n"),
            Some(TrackedPolicy::Refuse),
            "surrounding whitespace is not a different word"
        );
        assert_eq!(
            of("[packages]\ntracked_install = \"\"\n"),
            None,
            "blank is unset, like account"
        );
        assert_eq!(
            of("[packages]\ntracked_install = \"REFUSE\"\n"),
            None,
            "case-sensitive, like [machine] universal_control"
        );
        let typo = parse_packages("[packages]\nexclude = [\"ay\"]\ntracked_install = \"refuze\"\n");
        assert_eq!(
            typo.tracked_install(),
            None,
            "an unknown word is dropped at load — to unset, never read as refuse"
        );
        assert_eq!(
            typo.tracked_install, None,
            "dropped from the table itself, so no later consumer sees or re-reports it"
        );
        assert_eq!(
            typo.exclude(),
            ["ay".to_string()],
            "the rest of the table survives the one bad key"
        );
        assert_eq!(
            parse_packages("[packages]\ntracked_install = true\n").tracked_install(),
            None,
            "the wrong type is the malformed-file posture: loud defaults, no panic"
        );
        // A table built by hand and never parsed resolves the same way, silently.
        let hand = PackagesConfig {
            tracked_install: Some("nonsense".into()),
            ..PackagesConfig::default()
        };
        assert_eq!(hand.tracked_install(), None);
    }

    #[cfg(unix)]
    #[test]
    fn config_fifo_returns_defaults_and_symlinked_config_remains_supported() {
        use std::os::unix::ffi::OsStrExt as _;

        let root =
            std::env::temp_dir().join(format!("atpkg-config-admission-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
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
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn oversized_sparse_config_returns_bounded_defaults() {
        let root =
            std::env::temp_dir().join(format!("atpkg-config-oversized-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("aterm.toml");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len((MAX_PACKAGES_CONFIG_BYTES + 1) as u64)
            .unwrap();
        assert!(load_from_path(&path).exclude().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    /// `[reroute].announce` (2026-09-23, replacing `ATERM_REROUTE_QUIET`): default on,
    /// `false` silences, and a table that does not parse announces.
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

    #[test]
    fn classify_link_covers_the_three_shapes() {
        let home = Path::new("/home/u");
        // Absolute path → checkout.
        assert_eq!(
            classify_link("/src/ay", Some(home)),
            LinkTarget::Checkout(PathBuf::from("/src/ay"))
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
        // ~ with no resolvable home fails CLOSED, and ~user is unsupported.
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

    /// `[packages].prefix` shapes. Trust is NOT decided here — `store::vet_prefix`
    /// owns that and fails closed — so this only pins what the string resolves to.
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
            of(Some("/opt/aterm/pkg")),
            Some(PathBuf::from("/opt/aterm/pkg")),
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
