// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! ALAB-BUILT AGENT PROGRAMS: an agent program ([`crate::stub::AGENT_PROGRAMS`]) that ALab
//! builds itself on some targets and the signed index publishes there, beside the vendor's
//! own lane everywhere else. Today one: `codex` on `aarch64-apple-darwin` is codex-alab,
//! ALab's drop-in Codex (`alab/package.sh` in codex-alab: package layout v1, a
//! `codex-package.json` and an `alab-build.json` at its root, a pre-release version
//! `X.Y.0-dev.<n>+g<sha>`).
//!
//! THE SOURCE RULE ([`rule`]) is the one place that says which lane owns such a program on
//! this machine, and every branch point that used to ask only [`crate::vendor_direct::is_vendor`]
//! asks it ([`source`]):
//!
//! 1. dev-linked: the link (as for every program);
//! 2. a person chose the vendor's build here ([`vendor_chosen`], `aterm pkg update codex
//!    --vendor`): the vendor lane, until `aterm pkg update codex --alab` takes it back;
//! 3. the target is one ALab builds it for, the verified index names the program from
//!    ALab's repo ([`AlabAgent::repo`], never the legacy `aterm` row) and pins a build in the
//!    ALab store window ([`in_store_window`]) that no yank or floor revokes: the index lane;
//! 4. the active build is an ALab build ([`build_is_alab`], by its package marker, on a
//!    target ALab builds it for; read off the `current` link when a tombstone hides it):
//!    kept — never decided against the vendor's head, never replaced by it unattended or
//!    asked. A build the signed policy revokes (a yank, or below the floor) rolls back onto
//!    an older ALab build nothing revokes, else its commands are disabled, as the index
//!    lane disables a revoked build;
//! 5. otherwise: the vendor lane, exactly as before this table existed.
//!
//! WHY NO VERSION ORDER CROSSES THE LANES. codex-alab's version is a pre-release of the next
//! minor (`0.164.0-dev.N`), which SemVer puts BELOW the vendor's `0.164.0` once that ships:
//! "newer" points the wrong way at times, so it is never asked. The ALab lane orders by the
//! signed build number alone, and the source is chosen by the rule above, never by which
//! build is newer.
//!
//! WHAT FAILS CLOSED. An index that does not verify or has expired pins nothing here, so an
//! installed ALab build is kept and a vendor build stays on its vendor's lane. A wrong repo
//! string, another target, a pin outside the window or a revoked pin is the vendor lane for
//! a machine with no ALab build, and the kept ALab build for one that has it.

use std::path::{Path, PathBuf};

use crate::manifest::{Channel, Index};
use crate::store::Layout;

/// The manifest every Codex package carries at its root (layoutVersion 1).
pub const PACKAGE_JSON: &str = "codex-package.json";

/// The file an ALab build carries in its package root beside [`PACKAGE_JSON`]: the mark of
/// bytes ALab built, not the vendor's.
pub const ALAB_BUILD_JSON: &str = "alab-build.json";

/// Bound on a package manifest or build record read.
const MAX_PACKAGE_JSON_BYTES: usize = 64 * 1024;

/// One agent program ALab builds itself on some targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AlabAgent {
    /// The program name (an [`crate::stub::AGENT_PROGRAMS`] name: it keeps its twin, its
    /// self-update intercept and its vendor row for the targets ALab does not build).
    pub program: &'static str,
    /// The repo the signed index must name for the program (`[programs.<program>].repo`)
    /// before its pin routes here. The live index's legacy row names `aterm`, so a legacy
    /// pin never does.
    pub repo: &'static str,
    /// The targets ALab builds it for. Everywhere else the vendor lane is untouched.
    pub targets: &'static [&'static str],
    /// The product's own name, for the lines that say where a build came from.
    pub product: &'static str,
    /// The upstream repository the product is built from, as a release page names it
    /// ([`Upstream::release_line`]).
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

/// THE ALAB STORE WINDOW: an ALab build id is above every legacy pin of the vendor's bytes
/// ([`LEGACY_PIN_CEILING`]) and below every vendor build id
/// ([`crate::vendor_direct::VENDOR_BUILD_BASE`]), so it is never read as a legacy build the
/// vendor lane replaces, nor as a vendor version. Outside it a pin routes nothing here.
#[must_use]
pub const fn in_store_window(build: u64) -> bool {
    STORE_WINDOW.0 < build && build < STORE_WINDOW.1
}

/// The newest legacy pin of the vendor's bytes an index carried for an ALab-built program:
/// `codex = 2026101501` on the legacy `aterm` row of `atpkg-index-71`, above
/// [`crate::vendor_direct::LEGACY_CEILING`] (the final legacy pin the vendor lane vouches
/// for). The window starts above it, so a row or a store build carrying that legacy pin is
/// never read as ALab's.
pub const LEGACY_PIN_CEILING: u64 = {
    let live = 2_026_101_501;
    if crate::vendor_direct::LEGACY_CEILING > live {
        crate::vendor_direct::LEGACY_CEILING
    } else {
        live
    }
};

/// The exclusive bounds of [`in_store_window`], `(LEGACY_PIN_CEILING, VENDOR_BUILD_BASE)`:
/// what the producer names when it refuses a build id outside them.
pub const STORE_WINDOW: (u64, u64) = (LEGACY_PIN_CEILING, crate::vendor_direct::VENDOR_BUILD_BASE);

/// One `codex-package.json`'s entrypoint (relative to the root): `None` unless
/// `layoutVersion` is 1 and the entrypoint is a relative path with no `..` — the harness's
/// own reading (`aterm_agent::harness::codex_alab::package_entrypoint`), held to it by a
/// cross-check test. Which package
/// names a binary needs no version, and an ALab build's is a pre-release no version reader
/// here parses, on purpose.
///
/// Relative means every component is a name (or `.`): `is_absolute` alone admits Windows'
/// rooted `\bin\codex` and drive-relative `C:codex`, which `root.join` resolves off the
/// root.
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

/// Whether `root` is an ALAB PACKAGE: its [`PACKAGE_JSON`] is layout 1 with an entrypoint
/// that is a regular file inside it, and [`ALAB_BUILD_JSON`] stands beside it as a regular
/// file. `false` for anything that cannot be looked at (fail closed: a build this cannot
/// read is not called ALab's, and the caller's other rules decide it).
#[must_use]
pub fn is_alab_package(root: &Path) -> bool {
    let Some((_, entry)) = manifest_of(root) else {
        return false;
    };
    std::fs::symlink_metadata(root.join(entry)).is_ok_and(|m| m.file_type().is_file())
        && std::fs::symlink_metadata(root.join(ALAB_BUILD_JSON))
            .is_ok_and(|m| m.file_type().is_file())
}

/// The version an ALab package says it is (`codex-package.json`'s `version`), verbatim —
/// for the lines that name a build, never for ordering one.
#[must_use]
pub fn package_version(root: &Path) -> Option<String> {
    let (text, _) = manifest_of(root)?;
    let v: aterm_json::Value = aterm_json::from_str(&text).ok()?;
    let version = v.get("version")?.as_str()?;
    (!version.is_empty() && version.len() <= 128 && !version.chars().any(char::is_control))
        .then(|| version.to_string())
}

/// Whether the executable `exe` runs ALab bytes: the package root is `exe`'s directory or the
/// one above it (canonically), that root is an ALab package ([`is_alab_package`]: its
/// manifest and marker read bounded and never through a link), and its entrypoint IS `exe`.
/// The harness's image fact (`aterm_agent::harness::codex_alab::alab_bytes`) answers
/// the same on every package an atpkg store or dev link holds, and never "not ALab" where
/// this says ALab; a cross-check test holds that.
#[must_use]
pub fn exe_is_alab(exe: &Path) -> bool {
    alab_root_of_exe(exe).is_some()
}

/// The ALab package root whose entrypoint IS `exe` ([`exe_is_alab`]'s reading), canonical.
fn alab_root_of_exe(exe: &Path) -> Option<PathBuf> {
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

/// The upstream Codex an ALab build is based on, as its [`ALAB_BUILD_JSON`] records it:
/// `"upstream": {"release": "rust-v0.163.0", "commits_since_release": 241, "commit": "<40
/// hex>", "commit_date": "2026-10-20T13:50:13Z"}`, the values `alab/package.sh` computes for
/// `CODEX_ALAB_UPSTREAM_VERSION` (the newest stable upstream tag whose branch point the build
/// has merged), `CODEX_ALAB_UPSTREAM_COMMIT` (the upstream commit it merged last), the
/// commits upstream's main carries from that branch point to it, and that commit's UTC
/// committer date. Read off the record, never by running the binary; a package from before
/// the field, or one whose field does not hold the release and the commit in that shape, has
/// none. The count and the date are optional (packages from 2026-10-21 carry no count), but
/// either one present in another shape makes the whole field none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Upstream {
    /// The upstream release tag, verbatim (`rust-v0.163.0`).
    pub release: String,
    /// The upstream commit merged, 40 lowercase hex digits.
    pub commit: String,
    /// How many commits upstream's main carries from the release's branch point to
    /// `commit`, when the record says.
    pub since: Option<u64>,
    /// The UTC day `commit` was committed (`2026-10-20`), when the record says.
    pub day: Option<String>,
}

impl Upstream {
    /// `based on upstream rust-v0.163.0+g0123456789ab`: the release, then the commit's
    /// first twelve digits.
    #[must_use]
    pub fn words(&self) -> String {
        format!(
            "based on upstream {}+g{}",
            self.release,
            &self.commit[..UPSTREAM_SHORT_COMMIT]
        )
    }

    /// The line a release page names the base with, as codex-alab's `alab/package.sh
    /// --upstream-base` prints it for the source release: `Based on upstream openai/codex
    /// rust-v0.163.0 + 241 commits (0123456789ab, 2026-10-20)`, `repo` the upstream
    /// ([`AlabAgent::upstream`]). The count and the day are said only when the record holds
    /// them.
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

/// One `alab-build.json`'s [`Upstream`]: `None` unless `upstream` is an object whose
/// `release` is a short printable name and whose `commit` is a full 40-digit hex sha, and
/// whose `commits_since_release` (a count) and `commit_date` (`YYYY-MM-DDTHH:MM:SSZ`) are
/// each absent or in that shape. Never a partial answer from a malformed record.
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

/// The [`Upstream`] the ALab package at `root` records ([`is_alab_package`], its record
/// read bounded and never through a link).
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

/// The [`Upstream`] of the ALab package whose entrypoint is `exe` ([`exe_is_alab`]): a
/// store build's binary, or a dev-linked package's.
#[must_use]
pub fn upstream_of_exe(exe: &Path) -> Option<Upstream> {
    upstream_of(&alab_root_of_exe(exe)?)
}

/// The [`Upstream`] of the copy `program`'s managed shim runs ([`crate::which`]): its store
/// build, or the package a dev link names. `None` for a program ALab does not build.
#[must_use]
pub fn program_upstream(layout: &Layout, program: &str) -> Option<Upstream> {
    agent(program)?;
    upstream_of_exe(&crate::which(layout, program)?)
}

/// Whether `program`'s store build `build` is an ALab build: a program ALab builds, a build
/// id that is not a vendor one, and a complete tree that is an ALab package
/// ([`is_alab_package`]). The classification every lane reads before it may call a build
/// legacy or decide it against a vendor's head.
#[must_use]
pub fn build_is_alab(layout: &Layout, program: &str, build: u64) -> bool {
    agent(program).is_some()
        && !crate::vendor_direct::is_vendor_build(build)
        && is_alab_package(&layout.build_dir(program, build))
}

/// `program`'s active build when it is an ALab build ([`build_is_alab`]) on a target ALab
/// builds it for (`triple`; elsewhere an ALab build cannot run, and the vendor lane judges it
/// as it judged any build before this table). Read off the `bin/` shims, else off the
/// `store/<program>/current` activation link: a tombstoned or missing shim hides the build
/// from the shims, and the build it hides is still ALab's — never a machine with no codex
/// for the vendor lane to install over.
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

/// `program`'s retained ALab store builds below `below`, newest first: what a yank of the
/// active one rolls back onto.
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
    /// Whether the index lane owns the program here. Every other source is the vendor
    /// lane's door: a dev link is its to report as it always was, and a kept ALab build is
    /// its to keep ([`crate::vendor_direct::lane`] answers it before any head is read).
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

/// THE SOURCE RULE (module doc), pure.
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

/// The ALab build `index`'s channel `ch` (already the view from `triple`,
/// [`Channel::for_target`]) pins for `program`: `None` unless ALab builds `program` for
/// `triple`, the index names it from [`AlabAgent::repo`], the pin is in the store window
/// ([`in_store_window`]), and no yank or floor of that channel revokes it.
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

/// [`rule`] over this machine: the link, the person's choice, `index` (a VERIFIED, unexpired
/// index the caller holds; `None` pins nothing) and the active build.
#[must_use]
pub fn source(layout: &Layout, program: &str, triple: &str, index: Option<&Index>) -> Source {
    facts_with(layout, program, triple, |p| {
        index.and_then(|index| index_pin(index, p, triple))
    })
}

/// [`source`] reading the pin off `ch`, a channel of `index` already viewed from `triple`
/// (the planners hold one).
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

/// Whether the index lane owns `program` here under `index` ([`source`] is
/// [`Source::Index`]).
#[must_use]
pub fn index_owns(layout: &Layout, program: &str, triple: &str, index: Option<&Index>) -> bool {
    matches!(source(layout, program, triple, index), Source::Index { .. })
}

/// `<prefix>/sources/<program>.vendor`: the person's choice of the vendor's build of an
/// ALab-built program on this machine ([`choose_vendor`]).
#[must_use]
pub fn vendor_choice_path(layout: &Layout, program: &str) -> PathBuf {
    layout
        .prefix
        .join("sources")
        .join(format!("{program}.vendor"))
}

/// Whether a person chose the vendor's build of `program` here. Only a regular file counts.
#[must_use]
pub fn vendor_chosen(layout: &Layout, program: &str) -> bool {
    agent(program).is_some()
        && std::fs::symlink_metadata(vendor_choice_path(layout, program))
            .is_ok_and(|m| m.file_type().is_file())
}

/// Record a person's choice of the vendor's build of `program` (`aterm pkg update <program>
/// --vendor`): the one way back to the vendor for a machine on ALab's build, and never
/// written by anything unattended.
///
/// The regular marker's presence is the choice; its bytes are never read. Creating it
/// exclusively publishes that choice at once, without overwriting an existing entry. A
/// repeated choice accepts only an opened regular, non-link marker and changes no bytes.
///
/// # Errors
/// The directory or marker could not be created, or an existing marker is not a regular
/// non-link file.
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

/// Forget that choice (`aterm pkg update <program> --alab`): whether one was recorded.
pub fn clear_vendor_choice(layout: &Layout, program: &str) -> bool {
    std::fs::remove_file(vendor_choice_path(layout, program)).is_ok()
}

/// The words that name an ALab build on a line: `ALab build 0.164.0-dev.12 (build
/// 2026101601)`, or `ALab build 2026101601` when its package names no version.
#[must_use]
pub fn build_words(layout: &Layout, program: &str, build: u64) -> String {
    let words = crate::state::ALAB_BUILD_WORDS;
    match package_version(&layout.build_dir(program, build)) {
        Some(version) => format!("{words}{version} (build {build})"),
        None => format!("{words}{build}"),
    }
}

/// The checkout-relative bin a dev link of an ALab-built `program` to a PACKAGE DIRECTORY
/// names: the package's entrypoint (`bin/codex`), so `[packages.links] codex = "<pkgdir>"`
/// links what `alab/package.sh` laid without a bin list. `None` when `checkout` is no
/// package of that program.
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
