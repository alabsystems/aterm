// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The `[packages]` table of the SAME `aterm.toml` the GUI reads (§11) — atpkg's
//! own config reader.
//!
//! The GUI parses the whole file into its `Config` (including a mirror
//! `PackagesConfig` it uses only for the background-loop gate); atpkg reads just
//! the `[packages]` table out of the identical file, so there is exactly ONE
//! user-facing config surface. **Env always wins over config** at every
//! consumption site: `ATPKG_ACCOUNT` beats `account`
//! ([`crate::discovery::resolve_account`] precedence), `ATPKG_REGISTRY` /
//! `ATPKG_INDEX_REPO` / `ATPKG_DISABLE` have no config counterpart and are read
//! directly from the environment. Nothing here is a trust input: the account is
//! slug-validated downstream, `include`/`exclude` are narrowing-only over the
//! SIGNED index ([`crate::manifest::Index::installable`]), and a
//! `[packages.links]` entry can only redirect WHERE bytes are fetched from or
//! suppress registry management — never what verifies (§5/§8: the host is not an
//! authenticity input).
//!
//! A missing file, or a file without a `[packages]` table, yields the `[packages]`
//! defaults: no auto-install, no links, compiled account — the posture of a machine
//! that never wrote a config, which is most of them.
//!
//! A table we could not READ is NOT that case, and used to be treated as it.
//! CORRECTION 2026-09-16: this module claimed every `[packages]` default was the
//! inert/narrowest behavior, so a broken config "can only DISABLE bootstrap/links,
//! never widen anything". TWO defaults widen. `seed_install` defaults TRUE (§9.1), and
//! on a LEAN install that lane is a multi-GB DOWNLOAD; `exclude` defaults EMPTY, which
//! excludes nothing. So the owner who wrote `seed_install = false`, or
//! `exclude = ["trust"]`, had precisely the two keys that PREVENT large downloads reset
//! to the permissive reading by a typo — a wrong TYPE in the table, or a TOML syntax
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
    pub prefix: Option<String>,
    /// Master for the background tools loop (the GUI's `spawn_pkg_update_check`).
    /// Default TRUE (today's behavior). Read by the GUI, not by atpkg's own
    /// verbs — an explicit `atpkg update` always works regardless.
    pub enabled: Option<bool>,
    /// Run `atpkg update` on the background cadence. Default TRUE (today's
    /// behavior). Read by the GUI loop gate.
    pub auto_update: Option<bool>,
    /// Bootstrap the index default set over the NETWORK on a machine that has
    /// not adopted the toolset (§11). Default FALSE — pulling a multi-GB
    /// toolchain onto a machine that has never had one needs explicit consent,
    /// and the Settings switch is that click.
    ///
    /// This is NOT the switch that keeps an existing toolset complete. Once a
    /// machine has ADOPTED the set — the batteries-included seed bootstrap, or
    /// an explicit `install --default-set` — newly published members arrive on
    /// the ordinary update pass regardless of this bit
    /// ([`crate::store::Layout::adopted`]). Conflating the two made a user's
    /// toolchain decay into "whatever was published on install day".
    pub auto_install: Option<bool>,
    /// Install the BUNDLED seed registry on the first-run `atpkg seed` pass
    /// (§9.1 batteries-included). Default TRUE — the seed's bytes are already
    /// on disk, sealed under the app's own code signature, so installing the
    /// app is the consent for laying them down; the download-consent switch
    /// (`auto_install`) keeps gating the NETWORK bootstrap unchanged. `false`
    /// turns the first run back into an announced offer (Settings ▸ Packages).
    pub seed_install: Option<bool>,
    /// Index owner override (e.g. `"alabsystems"`). Default = the compiled
    /// owner; `ATPKG_ACCOUNT` env beats this ([`crate::discovery::resolve_account`]).
    /// Slug-validated downstream — a malformed value can never redirect fetches.
    pub account: Option<String>,
    /// The channel whose pin set drives install/update. Default `"stable"`.
    pub channel: Option<String>,
    /// Narrowing-only include filter over the index default set (§5): an entry
    /// the signed index does not name adds NOTHING. Default = every named program.
    pub include: Option<Vec<String>>,
    /// Narrowing-only exclude filter (subtracts after `include`).
    pub exclude: Option<Vec<String>>,
    /// `[packages].tracked_install`: what a provenance-tracked pass does when its
    /// untracked launchd lane cannot run ([`crate::lay`], "The policy when the lane
    /// cannot run") — `"record"` writes the files in-process, tagged, and records it
    /// beside the build (the default since 2026-09-14); `"refuse"` fails the install
    /// instead, naming why the lane failed. Default `record`.
    ///
    /// WHY A CONFIG KEY (2026-09-15): the knob was env-only
    /// (`ATPKG_REFUSE_TRACKED_INSTALL`), and the window's own update/seed passes are
    /// spawned from launchd's environment, which no shell export reaches — so a
    /// release-cutting machine that wanted the refusal could have it for a pass typed
    /// by hand and never for the unattended passes that lay most of its toolchain. This
    /// key is the durable spelling for a MACHINE; the env var, set non-empty in a
    /// shell, still wins for that one run ([`crate::lay::tracked_policy_of`]).
    /// Admitted at load ([`parse_packages`]): any word but the two is named once on
    /// stderr and treated as unset, so a typo falls to `record` — the reading that does
    /// not intervene — and never takes the rest of the table down with it.
    pub tracked_install: Option<String>,
    /// `[packages.links]`: `name = "/path/to/checkout"` (or `~/…`) declares a
    /// managed dev-link ([`crate::linkmode`] — registry management skipped);
    /// `name = "owner/repo"` declares a private-repo FETCH override for that
    /// program's release assets (signature verification UNCHANGED). Anything
    /// else is refused loudly ([`LinkTarget::Invalid`]).
    pub links: BTreeMap<String, String>,
    /// NOT a config key — `#[serde(skip)]`, so no `aterm.toml` can set it. True only of
    /// the table [`parse_packages`] could not read
    /// ([`PackagesConfig::unreadable_table`]). It is a bare `bool` in an otherwise
    /// all-`Option` struct because it is not a key: an absent key and a default-valued
    /// key are indistinguishable here on purpose, and this is neither. Every resolver
    /// whose default WIDENS consults it (see the module doc).
    #[serde(skip)]
    pub unreadable: bool,
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
    /// The channel to resolve pins from — `[packages].channel`, default `stable`.
    /// A blank value is treated as unset (never silently select a "" channel).
    #[must_use]
    pub fn channel(&self) -> &str {
        match self.channel.as_deref().map(str::trim) {
            Some(c) if !c.is_empty() => c,
            _ => "stable",
        }
    }

    /// The `[packages].account` override for [`crate::discovery::resolve_account`]
    /// (`None` ⇒ compiled default; `ATPKG_ACCOUNT` env still beats this).
    #[must_use]
    pub fn account(&self) -> Option<&str> {
        self.account
            .as_deref()
            .map(str::trim)
            .filter(|a| !a.is_empty())
    }

    /// Whether the `update` pass ALSO bootstraps missing default-set members.
    /// Default FALSE (explicit consent — §11).
    #[must_use]
    pub fn auto_install(&self) -> bool {
        self.auto_install.unwrap_or(false)
    }

    /// Whether the first-run `seed` pass INSTALLS the bundled registry rather
    /// than announcing it. Default TRUE (§9.1). On a SEALED install the bytes
    /// are local, under the app's own signature — installing the app is the
    /// consent. On a LEAN install (no seal: an Intel Mac, the zip container,
    /// Linux) the same lane resolves the signed NETWORK index and the "seed"
    /// is a multi-GB download; this key is the one switch that prevents it,
    /// so every user-facing description of it must say both halves (the old
    /// rationale here claimed local bytes unconditionally — audit-2 item 7).
    ///
    /// A table we could not READ resolves FALSE here, whatever the file said: the
    /// default is the one that costs gigabytes, and `seed_install = false` is the one
    /// documented way to say "not on my disk" — a typo elsewhere in `aterm.toml` must
    /// not repeal it ([`PackagesConfig::unreadable_table`]). Declining also means the
    /// lane records no adoption, which is what keeps the network pass moments later
    /// from completing the set.
    #[must_use]
    pub fn seed_install(&self) -> bool {
        !self.unreadable && self.seed_install.unwrap_or(true)
    }

    /// The narrowing-only include filter (empty ⇒ the whole index default set).
    #[must_use]
    pub fn include(&self) -> &[String] {
        self.include.as_deref().unwrap_or(&[])
    }

    /// The narrowing-only exclude filter.
    #[must_use]
    pub fn exclude(&self) -> &[String] {
        self.exclude.as_deref().unwrap_or(&[])
    }

    /// What `[packages].tracked_install` selects — `Some(Refuse)` for `"refuse"`,
    /// `Some(Allow)` for `"record"` — or `None` when the key is absent, blank, or not one
    /// of the two spellings (which [`parse_packages`] already named on stderr and
    /// dropped; a table built by hand and never parsed gets the same `None`, silently).
    /// The default and the precedence against `ATPKG_REFUSE_TRACKED_INSTALL` are
    /// [`crate::lay::tracked_policy_of`]'s, not this method's — one place decides.
    #[must_use]
    pub fn tracked_install(&self) -> Option<crate::lay::TrackedPolicy> {
        tracked_install_of(self.tracked_install.as_deref()?)
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

/// The `[machine]` table — the machine settings the update/seed pass applies at first
/// open and keeps applied (R5, owner decisions 2026-09-10). Read by atpkg because atpkg
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
    /// — at the top of every seed/update/install pass, and by `aterm pkg machine
    /// apply`. Default `true`.
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
    /// `Leave` — the inert reading — and says so once on stderr, rather than acting on
    /// a machine setting over a word the owner did not write.
    #[must_use]
    pub fn universal_control(&self) -> UniversalControlPolicy {
        match self.universal_control.as_deref().map(str::trim) {
            None | Some("") | Some("off") => UniversalControlPolicy::Off,
            Some("leave") => UniversalControlPolicy::Leave,
            Some(other) => {
                eprintln!(
                    "atpkg: [machine] universal_control = {other:?} is not \"off\" or \
                     \"leave\" — leaving Universal Control alone"
                );
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

/// Parse the `[packages]` table out of full `aterm.toml` text. A file without the
/// table ⇒ defaults; a malformed file ⇒ the loud UNREADABLE reading
/// ([`PackagesConfig::unreadable_table`] — never aborts a verb, and never the plain
/// defaults, two of which WIDEN: see the module doc). A well-formed table then passes
/// [`admit_packages`], the per-key admission.
#[must_use]
pub fn parse_packages(text: &str) -> PackagesConfig {
    match aterm_toml::from_str::<PackagesOnly>(text) {
        Ok(root) => admit_packages(root.packages.unwrap_or_default()),
        Err(e) => {
            eprintln!(
                "atpkg: ignoring malformed aterm.toml [packages] config — not installing \
                 the toolset on its say-so (seed declined, the set is not completed this \
                 pass; what is installed still updates): {e}"
            );
            PackagesConfig::unreadable_table()
        }
    }
}

/// The per-key admission a WELL-FORMED `[packages]` table still gets, once, at load: a
/// `tracked_install` that is neither `"record"` nor `"refuse"` (a blank is merely unset)
/// is named on stderr and dropped from the table, so no resolver ever sees a word the
/// owner did not write and no consumer repeats the complaint ([`cached`] loads once; the
/// policy is consulted once per staged artifact and once per laying pass). It is the
/// loud-defaults posture of a malformed file narrowed to the one key — a typo here must
/// not zero `channel`/`include`/`exclude` — and, like `[machine] universal_control`'s
/// unknown word, it falls to the reading that does NOT intervene: `record`, the default,
/// never `refuse`. The owner's 2026-09-14 ruling is that a lane that cannot run must not
/// leave a machine with no toolchain unless the refusal was spelled exactly; the doctor
/// and the release cutter's pre-claim gate still name a tagged toolchain either way.
fn admit_packages(mut cfg: PackagesConfig) -> PackagesConfig {
    if let Some(raw) = cfg.tracked_install.as_deref()
        && !raw.trim().is_empty()
        && tracked_install_of(raw).is_none()
    {
        eprintln!(
            "atpkg: [packages] tracked_install = {raw:?} is not \"record\" or \"refuse\" — \
             ignoring it (a tracked pass whose untracked lane cannot run RECORDS the \
             install, the default; tracked_install = \"refuse\" or {}=1 refuses)",
            crate::lay::REFUSE_TRACKED_ENV
        );
        cfg.tracked_install = None;
    }
    cfg
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
            eprintln!(
                "atpkg: malformed aterm.toml — the [machine] settings are not applied, and \
                 Universal Control and Spotlight are left exactly as they are: {e}"
            );
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

/// The process-wide `[machine]` config, read ONCE per invocation like [`cached`].
#[must_use]
pub fn cached_machine() -> &'static MachineConfig {
    static CFG: std::sync::OnceLock<MachineConfig> = std::sync::OnceLock::new();
    CFG.get_or_init(load_machine)
}

/// The process-wide `[packages]` config, read ONCE per invocation (atpkg is a
/// short-lived CLI; there is no reload seam to keep coherent). Env always wins
/// over these values at each consumption site — `ATPKG_ACCOUNT` /
/// `ATPKG_REGISTRY` / `ATPKG_INDEX_REPO` / `ATPKG_DISABLE` are read directly
/// from the environment, never through here.
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
        let none = parse_machine("font_px = 12.0\n[packages]\nchannel = \"stable\"\n");
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
        let both = "[packages]\nchannel = \"nightly\"\n[machine]\nspotlight_noindex = false\n";
        assert_eq!(parse_packages(both).channel(), "nightly");
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
        let other = "[packages]\nchannel = \"nightly\"\n[machine]\nspotlight_noindex = 3\n";
        assert_eq!(
            parse_packages(other).channel(),
            "nightly",
            "a [machine] type error must not discard the [packages] table"
        );
        // A file that is not TOML at all: nothing readable said to act, so nothing acts.
        let broken = parse_machine("[machine\nnot toml");
        assert_eq!(broken.universal_control(), UniversalControlPolicy::Leave);
        assert!(!broken.spotlight_noindex());
    }

    #[test]
    fn absent_table_yields_inert_defaults() {
        let cfg = parse_packages("font_px = 12.0\n[matrix_rain]\nenabled = true\n");
        assert_eq!(cfg.channel(), "stable");
        assert_eq!(cfg.account(), None);
        assert!(
            !cfg.auto_install(),
            "auto_install must default OFF (consent)"
        );
        assert!(
            cfg.seed_install(),
            "seed_install must default ON (§9.1 — the bundled bytes are local and sealed; \
             installing the app is the consent)"
        );
        assert!(cfg.include().is_empty());
        assert!(cfg.exclude().is_empty());
        assert!(cfg.links.is_empty());
        assert_eq!(
            cfg.tracked_install(),
            None,
            "tracked_install absent is unset — the default (record) is lay's to apply"
        );
        // The GUI-facing loop flags default to today's behavior (on).
        assert_eq!(cfg.enabled, None);
        assert_eq!(cfg.auto_update, None);
    }

    #[test]
    fn full_table_parses_every_key() {
        let cfg = parse_packages(
            "[packages]\nenabled = true\nauto_update = false\nauto_install = true\n\
             seed_install = false\n\
             account = \"alabsystems\"\nchannel = \"nightly\"\n\
             include = [\"ay\", \"trust\"]\nexclude = [\"trust\"]\n\
             tracked_install = \"refuse\"\n\
             [packages.links]\nay = \"~/ay\"\norc = \"alabsystems/orc\"\n",
        );
        assert_eq!(cfg.enabled, Some(true));
        assert_eq!(cfg.auto_update, Some(false));
        assert!(cfg.auto_install());
        assert!(!cfg.seed_install());
        assert_eq!(cfg.account(), Some("alabsystems"));
        assert_eq!(cfg.channel(), "nightly");
        assert_eq!(cfg.include(), ["ay".to_string(), "trust".to_string()]);
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

    // Malformed config ⇒ the loud UNREADABLE reading, never a panic/abort.
    #[test]
    fn malformed_config_falls_back_to_defaults() {
        let cfg = parse_packages("[packages\nnot toml");
        assert_eq!(cfg.channel(), "stable");
        assert!(!cfg.auto_install());
        assert!(cfg.links.is_empty());
        assert!(cfg.unreadable, "the table was not read");
        // A [packages] table of the WRONG SHAPE (scalar) also fails to defaults.
        let cfg = parse_packages("packages = 3\n");
        assert_eq!(cfg.channel(), "stable");
        assert!(cfg.unreadable);
    }

    /// A `[packages]` table we could not read must not WIDEN — the one claim this
    /// module made about malformed files that was FALSE (2026-09-16 audit).
    ///
    /// `seed_install` defaults TRUE and `exclude` defaults EMPTY, so resetting the table
    /// to its defaults switched ON the multi-GB paths those two keys exist to block: the
    /// owner of a lean install (an Intel Mac, a Linux box, any cut since 2026-08-26 —
    /// none seal a seed) who wrote `seed_install = false` got the seed lane installing
    /// anyway on the next launch, recording adoption, after which `cmd_update_all` sees
    /// `complete_the_set` and pulls the whole set over the network with `auto_install`
    /// still false. The trigger needs no mistake in `[packages]` at all: the GUI owns
    /// most of `aterm.toml`, and a syntax error anywhere in it fails this parse too.
    #[test]
    fn an_unreadable_packages_table_never_widens_the_download_gates() {
        // A wrong TYPE inside [packages] — the whole table fails, as
        // `tracked_install = true` already showed — with seed_install = false written
        // right above it.
        let typed = parse_packages("[packages]\nseed_install = false\nauto_install = \"yes\"\n");
        assert!(typed.unreadable);
        assert!(
            !typed.seed_install(),
            "a table we could not read is not consent to lay the toolset down: the \
             resolver's TRUE default is the expensive one, and on a lean install it is a \
             multi-GB download the owner declined in writing"
        );
        // A TOML syntax error in the GUI-OWNED part of the same file: [packages] itself
        // is spotless and still unreadable, because the document never parses.
        let elsewhere = parse_packages("[packages]\nseed_install = false\n[machine\nbroken\n");
        assert!(elsewhere.unreadable);
        assert!(!elsewhere.seed_install());
        // And exclude, the other widening default: it cannot be recovered, so the flag
        // is what the set-completion lane reads instead (`cli::should_complete_set`).
        let excluded = parse_packages("[packages]\nexclude = [\"trust\"]\nchannel = 7\n");
        assert!(excluded.unreadable);
        assert!(
            excluded.exclude().is_empty(),
            "the exclusions are genuinely gone — which is exactly why the pass that \
             would act on their absence must not run"
        );
        // A READABLE table is untouched by any of this: the default stays TRUE (§9.1).
        let fine = parse_packages("[packages]\nchannel = \"stable\"\n");
        assert!(!fine.unreadable);
        assert!(fine.seed_install());
        // A hand-built table (never parsed) is readable by construction.
        assert!(!PackagesConfig::default().unreadable);
        assert!(PackagesConfig::default().seed_install());
        assert!(PackagesConfig::unreadable_table().unreadable);
        assert!(!PackagesConfig::unreadable_table().seed_install());
    }

    /// `[packages].tracked_install` (2026-09-15): the two spellings resolve; absent and
    /// blank are unset (the env-var precedence and the `record` default are
    /// `lay::tracked_policy_of`'s); a word the owner did not write is dropped AT LOAD —
    /// to unset, never to `refuse` — and the rest of the table survives it; a value of
    /// the wrong TYPE is the whole table's malformed-file posture (loud defaults). Never
    /// a panic in any of these.
    #[test]
    fn tracked_install_admits_two_spellings_and_drops_the_rest_at_load() {
        use crate::lay::TrackedPolicy;
        let of = |toml: &str| parse_packages(toml).tracked_install();
        assert_eq!(of(""), None);
        assert_eq!(of("[packages]\nchannel = \"stable\"\n"), None);
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
            "blank is unset, like channel/account"
        );
        assert_eq!(
            of("[packages]\ntracked_install = \"REFUSE\"\n"),
            None,
            "case-sensitive, like [machine] universal_control"
        );
        let typo =
            parse_packages("[packages]\nchannel = \"nightly\"\ntracked_install = \"refuze\"\n");
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
            typo.channel(),
            "nightly",
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

    // Channel resolver: default, explicit, and blank-is-unset.
    #[test]
    fn channel_resolves_with_default_and_blank_guard() {
        assert_eq!(parse_packages("").channel(), "stable");
        assert_eq!(
            parse_packages("[packages]\nchannel = \"nightly\"\n").channel(),
            "nightly"
        );
        assert_eq!(
            parse_packages("[packages]\nchannel = \"  \"\n").channel(),
            "stable",
            "a blank channel is treated as unset, never a \"\" channel lookup"
        );
    }

    // Account precedence env > config > default, via the pure discovery split
    // (no process-env mutation).
    #[test]
    fn account_precedence_env_beats_config_beats_default() {
        let cfg = parse_packages("[packages]\naccount = \"alabsystems\"\n");
        // config beats the compiled default…
        assert_eq!(
            crate::resolve_account_with(None, cfg.account()).owner,
            "alabsystems"
        );
        // …env beats config…
        assert_eq!(
            crate::resolve_account_with(Some("env-org"), cfg.account()).owner,
            "env-org"
        );
        // …and an invalid config account can never redirect (falls to default).
        // ATPKG_INDEX_OWNER, not PUBLISH_OWNER or DEFAULT_OWNER: the package
        // index has its own tracked owner key (the public account) — it follows
        // neither the private staging repo nor the updater's mirror channel.
        let bad = parse_packages("[packages]\naccount = \"evil.com/x\"\n");
        assert_eq!(
            crate::resolve_account_with(None, bad.account()).owner,
            aterm_update_core::ATPKG_INDEX_OWNER
        );
        // Blank config account is treated as unset.
        let blank = parse_packages("[packages]\naccount = \"\"\n");
        assert_eq!(blank.account(), None);
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
        assert_eq!(
            load_from_path(&fifo).channel(),
            "stable",
            "a writerless config FIFO must return the finite default immediately"
        );

        let target = root.join("target.toml");
        let logical = root.join("aterm.toml");
        std::fs::write(&target, "[packages]\nchannel = \"nightly\"\n").unwrap();
        std::os::unix::fs::symlink(&target, &logical).unwrap();
        assert_eq!(load_from_path(&logical).channel(), "nightly");
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
        assert_eq!(load_from_path(&path).channel(), "stable");
        let _ = std::fs::remove_dir_all(root);
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
        let cfg = parse_packages(
            "[packages.links]\nay = \"/src/ay\"\norc = \"alabsystems/orc\"\n\
             bad = \"evil host/x\"\n",
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
