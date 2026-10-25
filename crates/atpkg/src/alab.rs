// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! ALAB-BUILT AGENT PROGRAMS.

use std::path::{Path, PathBuf};

use crate::manifest::{Channel, Index};
use crate::store::Layout;

/// The manifest every Codex package carries at its root (layoutVersion 1).
pub const PACKAGE_JSON: &str = "codex-package.json";

/// The file an ALab build carries in its package root beside [`PACKAGE_JSON`].
pub const ALAB_BUILD_JSON: &str = "alab-build.json";

/// Bound on a package manifest or build record read.
const MAX_PACKAGE_JSON_BYTES: usize = 64 * 1024;

/// One agent program ALab builds itself on some targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AlabAgent {
    /// The program name ([`crate::stub::AGENT_PROGRAMS`]).
    pub program: &'static str,
    /// The repo the signed index must name for the program (`[programs.<program>].repo`) before its pin routes here.
    pub repo: &'static str,
    /// The targets ALab builds it for.
    pub targets: &'static [&'static str],
    /// The product's own name, for the lines that say where a build came from.
    pub product: &'static str,
    /// The upstream repository the product is built from, as a release page names it ([`Upstream::release_line`]).
    pub upstream: &'static str,
}

impl AlabAgent {
    /// Whether ALab builds this program for `triple`.
    #[must_use]
    pub fn serves(&self, triple: &str) -> bool {
        self.targets.contains(&triple)
    }
}

/// The agent programs ALab builds itself.
pub const ALAB_AGENTS: &[AlabAgent] = &[AlabAgent {
    program: "codex",
    repo: "codex-alab",
    targets: &["aarch64-apple-darwin"],
    product: "codex-alab",
    upstream: "openai/codex",
}];

/// The row for `program`, or `None` for a program ALab does not build.
#[must_use]
pub fn agent(program: &str) -> Option<&'static AlabAgent> {
    ALAB_AGENTS.iter().find(|a| a.program == program)
}

/// THE ALAB STORE WINDOW.
#[must_use]
pub const fn in_store_window(build: u64) -> bool {
    STORE_WINDOW.0 < build && build < STORE_WINDOW.1
}

/// The newest legacy pin of the vendor's bytes an index carried for an ALab-built program.
pub const LEGACY_PIN_CEILING: u64 = {
    let live = 2_026_101_501;
    if crate::vendor_direct::LEGACY_CEILING > live {
        crate::vendor_direct::LEGACY_CEILING
    } else {
        live
    }
};

/// The exclusive bounds of [`in_store_window`], `(LEGACY_PIN_CEILING, VENDOR_BUILD_BASE)`.
pub const STORE_WINDOW: (u64, u64) = (LEGACY_PIN_CEILING, crate::vendor_direct::VENDOR_BUILD_BASE);

/// One `codex-package.json`'s entrypoint (relative to the root).
#[must_use]
pub fn package_entrypoint(text: &str) -> Option<PathBuf> {
    use std::path::Component;
    let v: aterm_json::Value = aterm_json::from_str(text).ok()?;
    if v.get("layoutVersion").and_then(aterm_json::Value::as_u64) != Some(1) {
        return None;
    }
    let entry = Path::new(v.get("entrypoint")?.as_str()?);
    if !entry
        .components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return None;
    }
    Some(entry.to_path_buf())
}

/// The package root `root`'s manifest text and entrypoint, read bounded and never through a
/// link.
fn manifest_of(root: &Path) -> Option<(String, PathBuf)> {
    let text = crate::metadata_io::read_bounded_regular_utf8(
        &root.join(PACKAGE_JSON),
        MAX_PACKAGE_JSON_BYTES,
    )
    .ok()?;
    let entry = package_entrypoint(&text)?;
    Some((text, entry))
}

/// The entrypoint package root `root`'s manifest names ([`package_entrypoint`]).
#[must_use]
pub fn entrypoint_of(root: &Path) -> Option<PathBuf> {
    manifest_of(root).map(|(_, entry)| entry)
}

/// Whether `root` is an ALAB PACKAGE.
#[must_use]
pub fn is_alab_package(root: &Path) -> bool {
    let Some((_, entry)) = manifest_of(root) else {
        return false;
    };
    std::fs::symlink_metadata(root.join(entry)).is_ok_and(|m| m.file_type().is_file())
        && std::fs::symlink_metadata(root.join(ALAB_BUILD_JSON))
            .is_ok_and(|m| m.file_type().is_file())
}

/// The version an ALab package says it is (`codex-package.json`'s `version`), verbatim.
#[must_use]
pub fn package_version(root: &Path) -> Option<String> {
    let (text, _) = manifest_of(root)?;
    let v: aterm_json::Value = aterm_json::from_str(&text).ok()?;
    let version = v.get("version")?.as_str()?;
    (!version.is_empty() && version.len() <= 128 && !version.chars().any(char::is_control))
        .then(|| version.to_string())
}

/// A CODEX-ALAB VERSION, in one of the four shapes codex-alab's `alab/package.sh` mints
/// (codex-alab `alab/README.md`, Decision 8):
///
/// * `X.Y.Z`, a bare release (the upstream-numbered `0.164.0` of 2026-10-16);
/// * `X.Y.Z+codex.<A.B.C>`, a COMPOUND release: codex-alab's own core with the upstream Codex
///   baseline it is built on in SemVer build metadata;
/// * `X.Y.Z-dev.<count>+g<sha9>`, a dev build;
/// * `X.Y.Z-dev.<count>+codex.<A.B.C>.g<sha9>`, a compound dev build.
///
/// Every number is decimal without a leading zero (SemVer's rule for the core, kept for the
/// baseline so one baseline has one spelling), at most nine digits; the sha is nine lowercase
/// hex digits. Any other pre-release or build metadata is refused, never guessed at. Build
/// metadata never orders ([`AlabVersion::precedence`]): codex-alab's own line does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AlabVersion<'a> {
    /// The core, `MAJOR.MINOR.PATCH`.
    pub core: (u64, u64, u64),
    /// A dev build's `<count>` (`git rev-list --count` of its source).
    pub dev: Option<u64>,
    /// The upstream Codex baseline, `+codex.<A.B.C>`: the newest upstream stable `rust-vA.B.C`
    /// whose branch point the build's source has merged.
    pub baseline: Option<(u64, u64, u64)>,
    /// The baseline as the version spells it (`0.164.0`).
    pub baseline_text: Option<&'a str>,
    /// The source commit a dev build names, nine lowercase hex digits.
    pub sha9: Option<&'a str>,
}

/// The longest codex-alab version [`AlabVersion::parse`] reads (as [`package_version`] bounds one).
const MAX_ALAB_VERSION_BYTES: usize = 128;

/// One canonical decimal: digits only, no leading zero, at most nine digits.
fn canonical_number(part: &str) -> Option<u64> {
    let digits = !part.is_empty() && part.len() <= 9 && part.bytes().all(|b| b.is_ascii_digit());
    (digits && (part == "0" || !part.starts_with('0')))
        .then(|| part.parse().ok())
        .flatten()
}

/// `A.B.C`, each part a [`canonical_number`].
fn canonical_triple(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.split('.');
    let version = (
        canonical_number(parts.next()?)?,
        canonical_number(parts.next()?)?,
        canonical_number(parts.next()?)?,
    );
    parts.next().is_none().then_some(version)
}

impl<'a> AlabVersion<'a> {
    /// `text` as a codex-alab version, or `None` for any shape the type does not name.
    #[must_use]
    pub fn parse(text: &'a str) -> Option<Self> {
        if text.len() > MAX_ALAB_VERSION_BYTES {
            return None;
        }
        let (rest, metadata) = match text.split_once('+') {
            Some((rest, metadata)) => (rest, Some(metadata)),
            None => (text, None),
        };
        let (core, dev) = match rest.split_once('-') {
            Some((core, pre)) => (core, Some(canonical_number(pre.strip_prefix("dev.")?)?)),
            None => (rest, None),
        };
        let core = canonical_triple(core)?;
        let hex9 = |hex: &'a str| {
            (hex.len() == 9 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
                .then_some(hex)
        };
        let (baseline_text, sha9) = match metadata {
            None => (None, None),
            Some(m) => match m.strip_prefix("codex.") {
                // `codex.<A.B.C>`, a dev build's followed by `.g<sha9>`.
                Some(compound) => match compound.rsplit_once(".g") {
                    Some((baseline, hex)) if dev.is_some() => (Some(baseline), Some(hex9(hex)?)),
                    _ => (Some(compound), None),
                },
                None => (None, Some(hex9(m.strip_prefix('g')?)?)),
            },
        };
        // A dev build names its commit; a release names none.
        if dev.is_some() != sha9.is_some() {
            return None;
        }
        let baseline = match baseline_text {
            Some(b) => Some(canonical_triple(b)?),
            None => None,
        };
        Some(Self {
            core,
            dev,
            baseline,
            baseline_text,
            sha9,
        })
    }

    /// The SemVer precedence key: the core, then a dev build below its release, then the
    /// dev `<count>`. The baseline and the sha (build metadata) take no part in it.
    #[must_use]
    pub fn precedence(&self) -> ((u64, u64, u64), bool, u64) {
        (self.core, self.dev.is_none(), self.dev.unwrap_or(0))
    }

    /// `codex 0.164.0`: the upstream Codex baseline, as a line names it.
    #[must_use]
    pub fn baseline_words(&self) -> Option<String> {
        Some(format!("codex {}", self.baseline_text?))
    }
}

/// Whether the executable `exe` runs ALab bytes.
#[must_use]
pub fn exe_is_alab(exe: &Path) -> bool {
    alab_root_of_exe(exe).is_some()
}

/// The ALab package root whose entrypoint IS `exe` ([`exe_is_alab`]'s reading), canonical.
pub(crate) fn alab_root_of_exe(exe: &Path) -> Option<PathBuf> {
    let exe = std::fs::canonicalize(exe).ok()?;
    exe.ancestors()
        .skip(1)
        .take(2)
        .find(|root| {
            is_alab_package(root)
                && entrypoint_of(root)
                    .and_then(|entry| std::fs::canonicalize(root.join(entry)).ok())
                    .is_some_and(|entry| entry == exe)
        })
        .map(Path::to_path_buf)
}

/// The upstream Codex an ALab build is based on, as its [`ALAB_BUILD_JSON`] records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Upstream {
    /// The upstream release tag, verbatim (`rust-v0.163.0`).
    pub release: String,
    /// The upstream commit merged, 40 lowercase hex digits.
    pub commit: String,
    /// How many commits upstream's main carries from the release's branch point to `commit`, when the record says.
    pub since: Option<u64>,
    /// The UTC day `commit` was committed (`2026-10-20`), when the record says.
    pub day: Option<String>,
}

impl Upstream {
    /// `based on upstream rust-v0.163.0+g0123456789ab`.
    #[must_use]
    pub fn words(&self) -> String {
        format!(
            "based on upstream {}+g{}",
            self.release,
            &self.commit[..UPSTREAM_SHORT_COMMIT]
        )
    }

    /// The line a release page names the base with, as codex-alab's `alab/package.sh --upstream-base` prints it for the source release.
    #[must_use]
    pub fn release_line(&self, repo: &str) -> String {
        let since = self.since.map_or_else(String::new, |n| {
            format!(" + {n} {}", if n == 1 { "commit" } else { "commits" })
        });
        let day = self
            .day
            .as_ref()
            .map_or_else(String::new, |day| format!(", {day}"));
        format!(
            "Based on upstream {repo} {}{since} ({}{day})",
            self.release,
            &self.commit[..UPSTREAM_SHORT_COMMIT]
        )
    }
}

/// How many digits of the upstream commit [`Upstream::words`] says.
const UPSTREAM_SHORT_COMMIT: usize = 12;

/// One `alab-build.json`'s [`Upstream`].
#[must_use]
pub fn parse_upstream(text: &str) -> Option<Upstream> {
    let v: aterm_json::Value = aterm_json::from_str(text).ok()?;
    let upstream = v.get("upstream")?;
    let release = upstream.get("release")?.as_str()?;
    let commit = upstream.get("commit")?.as_str()?;
    let printable = |s: &str| s.chars().all(|c| c.is_ascii_graphic());
    let release_ok = !release.is_empty() && release.len() <= 128 && printable(release);
    let commit_ok = commit.len() == 40 && commit.bytes().all(|b| b.is_ascii_hexdigit());
    let since = match upstream.get("commits_since_release") {
        None => None,
        Some(n) => Some(n.as_u64()?),
    };
    let day = match upstream.get("commit_date") {
        None => None,
        Some(date) => Some(utc_day(date.as_str()?)?),
    };
    (release_ok && commit_ok).then(|| Upstream {
        release: release.to_string(),
        commit: commit.to_ascii_lowercase(),
        since,
        day,
    })
}

/// The day of a UTC time as `alab/package.sh` records one (`2026-10-20T13:50:13Z`), or
/// `None` for any other shape.
fn utc_day(time: &str) -> Option<String> {
    let shaped = time.len() == 20
        && time.bytes().enumerate().all(|(i, b)| match i {
            4 | 7 => b == b'-',
            10 => b == b'T',
            13 | 16 => b == b':',
            19 => b == b'Z',
            _ => b.is_ascii_digit(),
        });
    shaped.then(|| time[..10].to_string())
}

/// The [`Upstream`] the ALab package at `root` records ([`is_alab_package`], its record read bounded and never through a link).
#[must_use]
pub fn upstream_of(root: &Path) -> Option<Upstream> {
    if !is_alab_package(root) {
        return None;
    }
    let text = crate::metadata_io::read_bounded_regular_utf8(
        &root.join(ALAB_BUILD_JSON),
        MAX_PACKAGE_JSON_BYTES,
    )
    .ok()?;
    parse_upstream(&text)
}

/// The [`Upstream`] of the ALab package whose entrypoint is `exe` ([`exe_is_alab`]).
#[must_use]
pub fn upstream_of_exe(exe: &Path) -> Option<Upstream> {
    upstream_of(&alab_root_of_exe(exe)?)
}

/// AN ATERM VERSION as the minimum-aterm pairing compares one ([`pairs_with`]).
pub type AtermVersion = (u64, u64, u64);

/// The longest aterm version [`parse_aterm_version`] reads.
const MAX_ATERM_VERSION_BYTES: usize = 64;

/// `MAJOR.MINOR.PATCH`, ignoring any `-<pre-release>` or `+<build>` suffix.
#[must_use]
pub fn parse_aterm_version(text: &str) -> Option<AtermVersion> {
    if text.len() > MAX_ATERM_VERSION_BYTES {
        return None;
    }
    let core = text.split(['+', '-']).next()?;
    let mut parts = core.split('.');
    let mut number = || -> Option<u64> {
        let part = parts.next()?;
        if part.is_empty() || part.len() > 9 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        part.parse().ok()
    };
    let version = (number()?, number()?, number()?);
    parts.next().is_none().then_some(version)
}

/// `0.110.0`, as a line names an [`AtermVersion`].
#[must_use]
pub fn aterm_version_words(v: AtermVersion) -> String {
    format!("{}.{}.{}", v.0, v.1, v.2)
}

/// What one `alab-build.json` says of the aterm its build needs.
///
/// # Errors
/// The record is not JSON, or names an `aterm` in another shape.
pub fn parse_aterm_min(text: &str) -> Result<Option<AtermVersion>, String> {
    let v: aterm_json::Value =
        aterm_json::from_str(text).map_err(|_| "alab-build.json is not JSON".to_string())?;
    let Some(aterm) = v.get("aterm") else {
        return Ok(None);
    };
    aterm
        .get("min")
        .and_then(aterm_json::Value::as_str)
        .and_then(parse_aterm_version)
        .map(Some)
        .ok_or_else(|| {
            "alab-build.json names an aterm pairing in no shape this aterm reads (\"aterm\": \
             {\"min\": \"MAJOR.MINOR.PATCH\"})"
                .to_string()
        })
}

/// THE ATERM THE ALAB PACKAGE AT `root` NEEDS.
///
/// # Errors
/// `root` is no ALab package ([`is_alab_package`]), its record cannot be read, or the record names a pairing in another shape.
pub fn pairing_floor(root: &Path) -> Result<Option<AtermVersion>, String> {
    if !is_alab_package(root) {
        return Err("not an ALab package".to_string());
    }
    let text = crate::metadata_io::read_bounded_regular_utf8(
        &root.join(ALAB_BUILD_JSON),
        MAX_PACKAGE_JSON_BYTES,
    )
    .map_err(|e| format!("alab-build.json cannot be read: {e}"))?;
    parse_aterm_min(&text)
}

/// THIS ATERM's version.
#[must_use]
pub fn this_aterm() -> AtermVersion {
    parse_aterm_version(env!("CARGO_PKG_VERSION")).unwrap_or((0, 0, 0))
}

/// THE MINIMUM-ATERM PAIRING.
///
/// # Errors
/// Why not, in a line that names both versions.
pub fn pairs_with(root: &Path, aterm: AtermVersion) -> Result<(), String> {
    let needs = pairing_floor(root).map_err(|why| format!("alab pairing: {why}"))?;
    let Some(needs) = needs.filter(|&needs| aterm < needs) else {
        return Ok(());
    };
    Err(format!(
        "alab pairing: this ALab build needs aterm {} or later; this aterm is {} (update aterm, \
         then this build)",
        aterm_version_words(needs),
        aterm_version_words(aterm)
    ))
}

/// The [`Upstream`] of the copy `program`'s managed shim runs ([`crate::which`]).
#[must_use]
pub fn program_upstream(layout: &Layout, program: &str) -> Option<Upstream> {
    agent(program)?;
    upstream_of_exe(&crate::which(layout, program)?)
}

/// Whether `program`'s store build `build` is an ALab build.
#[must_use]
pub fn build_is_alab(layout: &Layout, program: &str, build: u64) -> bool {
    agent(program).is_some()
        && !crate::vendor_direct::is_vendor_build(build)
        && is_alab_package(&layout.build_dir(program, build))
}

/// `program`'s active ALab build ([`build_is_alab`]) on a supported `triple`.
#[must_use]
pub fn active_alab_build(layout: &Layout, program: &str, triple: &str) -> Option<u64> {
    if !agent(program)?.serves(triple) {
        return None;
    }
    let by_shim = crate::ops::active_builds(layout).get(program).copied();
    by_shim
        .or_else(|| current_build(layout, program))
        .filter(|build| build_is_alab(layout, program, *build))
}

/// The build `store/<program>/current` names, when it names one of `program`'s own.
#[must_use]
pub fn current_build(layout: &Layout, program: &str) -> Option<u64> {
    let target = std::fs::read_link(layout.program_current(program)).ok()?;
    let (owner, build) = crate::ops::store_build_of(&layout.prefix, &target)?;
    (owner == program).then_some(build)
}

/// `program`'s retained ALab store builds below `below`, newest first.
#[must_use]
pub fn retained_alab_below(layout: &Layout, program: &str, below: u64) -> Vec<u64> {
    let mut builds: Vec<u64> = crate::ops::list_installed(layout)
        .into_iter()
        .filter(|(p, b)| p == program && *b < below)
        .map(|(_, b)| b)
        .filter(|b| {
            let dir = layout.build_dir(program, *b);
            crate::store::build_is_complete(&dir) && build_is_alab(layout, program, *b)
        })
        .collect();
    builds.sort_unstable_by(|a, b| b.cmp(a));
    builds
}

/// Which lane owns an agent program here ([`rule`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Dev-linked: managed from its checkout.
    Linked,
    /// The signed index pins this ALab build for this target: the index lane.
    Index {
        /// The pinned build.
        build: u64,
    },
    /// The active build is ALab's and nothing pins one here: kept, never the vendor's.
    KeepAlab {
        /// The active build.
        build: u64,
    },
    /// The vendor's own lane, exactly as for a program ALab never built.
    VendorDirect,
}

impl Source {
    /// Whether the index lane owns the program here.
    #[must_use]
    pub const fn index_lane(self) -> bool {
        matches!(self, Self::Index { .. })
    }
}

/// What [`rule`] decides from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// The program is dev-linked here.
    pub linked: bool,
    /// A person chose the vendor's build here ([`vendor_chosen`]).
    pub vendor_chosen: bool,
    /// The ALab build the verified index pins for this target ([`channel_pin`]).
    pub index_pin: Option<u64>,
    /// The active build, when it is ALab's ([`active_alab_build`]).
    pub installed_alab: Option<u64>,
}

/// THE SOURCE RULE, pure.
#[must_use]
pub const fn rule(f: &Facts) -> Source {
    if f.linked {
        return Source::Linked;
    }
    if f.vendor_chosen {
        return Source::VendorDirect;
    }
    if let Some(build) = f.index_pin {
        return Source::Index { build };
    }
    if let Some(build) = f.installed_alab {
        return Source::KeepAlab { build };
    }
    Source::VendorDirect
}

/// The ALab build `index`'s channel `ch` (already the view from `triple`, [`Channel::for_target`]) pins for `program`.
#[must_use]
pub fn channel_pin(index: &Index, ch: &Channel, program: &str, triple: &str) -> Option<u64> {
    let agent = agent(program)?;
    if !agent.serves(triple) || index.program(program)?.repo != agent.repo {
        return None;
    }
    let &build = ch.pin.get(program)?;
    (in_store_window(build)
        && !crate::gate::is_yanked(ch, program, build)
        && build >= ch.min_build_for(program))
    .then_some(build)
}

/// [`channel_pin`] on `index`'s configured channel as `triple` sees it.
#[must_use]
pub fn index_pin(index: &Index, program: &str, triple: &str) -> Option<u64> {
    agent(program)?;
    let ch = index.channel_for(crate::config::CHANNEL, triple)?;
    channel_pin(index, &ch, program, triple)
}

/// [`rule`] over this machine.
#[must_use]
pub fn source(layout: &Layout, program: &str, triple: &str, index: Option<&Index>) -> Source {
    facts_with(layout, program, triple, |p| {
        index.and_then(|index| index_pin(index, p, triple))
    })
}

/// [`source`] reading the pin off `ch`, a channel of `index` already viewed from `triple` (the planners hold one).
#[must_use]
pub fn source_in(
    layout: &Layout,
    index: &Index,
    ch: &Channel,
    program: &str,
    triple: &str,
) -> Source {
    facts_with(layout, program, triple, |p| {
        channel_pin(index, ch, p, triple)
    })
}

/// [`rule`] over this machine with the index's pin read by `pin`.
fn facts_with(
    layout: &Layout,
    program: &str,
    triple: &str,
    pin: impl FnOnce(&str) -> Option<u64>,
) -> Source {
    let linked = crate::linkmode::is_linked(layout, program);
    if agent(program).is_none() {
        return rule(&Facts {
            linked,
            ..Facts::default()
        });
    }
    rule(&Facts {
        linked,
        vendor_chosen: vendor_chosen(layout, program),
        index_pin: pin(program),
        installed_alab: active_alab_build(layout, program, triple),
    })
}

/// Whether the index lane owns `program` here under `index` ([`source`] is [`Source::Index`]).
#[must_use]
pub fn index_owns(layout: &Layout, program: &str, triple: &str, index: Option<&Index>) -> bool {
    matches!(source(layout, program, triple, index), Source::Index { .. })
}

/// `<prefix>/sources/<program>.vendor`.
#[must_use]
pub fn vendor_choice_path(layout: &Layout, program: &str) -> PathBuf {
    layout
        .prefix
        .join("sources")
        .join(format!("{program}.vendor"))
}

/// Whether a person chose the vendor's build of `program` here.
#[must_use]
pub fn vendor_chosen(layout: &Layout, program: &str) -> bool {
    agent(program).is_some()
        && std::fs::symlink_metadata(vendor_choice_path(layout, program))
            .is_ok_and(|m| m.file_type().is_file())
}

/// Record a person's choice of the vendor's build of `program` (`aterm pkg update <program> --vendor`).
///
/// # Errors
/// The directory or marker could not be created, or an existing marker is not a regular non-link file.
pub fn choose_vendor(layout: &Layout, program: &str) -> std::io::Result<()> {
    let path = vendor_choice_path(layout, program);
    if let Some(dir) = path.parent() {
        layout.ensure_dir(dir)?;
    }
    match crate::platform::create_private_new(&path) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            crate::metadata_io::open_regular(&path).map(drop)
        }
        Err(e) => Err(e),
    }
}

/// Forget that choice (`aterm pkg update <program> --alab`).
pub fn clear_vendor_choice(layout: &Layout, program: &str) -> bool {
    std::fs::remove_file(vendor_choice_path(layout, program)).is_ok()
}

/// The words that name an ALab build on a line.
#[must_use]
pub fn build_words(layout: &Layout, program: &str, build: u64) -> String {
    let words = crate::state::ALAB_BUILD_WORDS;
    match package_version(&layout.build_dir(program, build)) {
        Some(version) => format!("{words}{version} (build {build})"),
        None => format!("{words}{build}"),
    }
}

/// The checkout-relative bin a dev link of an ALab-built `program` to a PACKAGE DIRECTORY names.
#[must_use]
pub fn package_link_bin(program: &str, checkout: &Path) -> Option<PathBuf> {
    agent(program)?;
    let (_, entry) = manifest_of(checkout)?;
    std::fs::symlink_metadata(checkout.join(&entry))
        .is_ok_and(|m| m.file_type().is_file())
        .then_some(entry)
}

#[cfg(test)]
#[path = "alab_tests.rs"]
pub(crate) mod tests;
