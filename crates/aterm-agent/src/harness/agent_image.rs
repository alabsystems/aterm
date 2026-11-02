// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE AGENT IDENTITY LAW: a name steers, the kernel confirms, and an agent nothing confirms is
//! observed, never driven.
//!
//! A foreground program's name (`argv[0]`) says which agent's reader reads its screen, what
//! `status program=` prints and what the session log calls its run. It confirms nothing: any
//! program can be run under the name `claude` (`exec -a`, a link, a copy, a script). The harness
//! types into an agent only when what the kernel keeps for the running image confirms it is that
//! agent ([`proof`]), by one of:
//!
//! 1. [`Proof::Layout`]: the image is an install of that agent by its vendor's layout
//!    ([`in_vendor_layout`], the one per-agent predicate relaunch and the claim both call): a
//!    Claude Code under `~/.local/share/claude/versions/<version>` (that directory's own entry,
//!    a regular file, is the mapped file), an atpkg store build registered for that agent, a
//!    codex-alab package.
//! 2. [`Proof::Team`]: on macOS, the running process's signature names the agent vendor's
//!    signing team ([`vendor_team`]) where the kernel validated it as Developer ID
//!    (`CS_OPS_VALIDATION_CATEGORY`: its certificate chain is Apple's Developer ID authority's;
//!    an ad hoc, linker or local signature is in no such category) and the running code's
//!    certificate check holds its leaf certificate to that team (the host's
//!    `agent_act::live_team`). The kernel's status flags cannot tell an ad hoc signature: it
//!    reports no `CS_ADHOC` for one (measured 2026-11-01).
//! 3. [`Proof::Seam`]: in a build with debug assertions, the lab's stand-in agents, named by the
//!    development seam [`STANDINS_SEAM`] and compared by file identity.
//!
//! A `#!` or node shim whose script is the agent's own (node running an npm package's `cli.js`)
//! is not confirmed: no reader confirms such a script by identity yet (the SPECS gap). Nor is an
//! image unlinked since its exec, where no layout entry is the file the kernel mapped any more
//! and no signature vouches (Linux): it is observed, the law's verdict for what nothing proves.
//! Where no file identity can be read ([`FileId`]), nothing confirms by identity.
//! [`Act::Observe`] is what an unconfirmed agent gets: the claim stands and its boxes are read,
//! logged and escalated to the person, and nothing is typed.

use std::path::{Path, PathBuf};

use aterm_phase::Program;
use atpkg::image::{ImageRoots, ImageSource};

/// Claude Code's signing team: `TeamIdentifier=Q6L2SF6YDW` (`codesign -dv`, keys only, on the
/// native 2.1.227, 2.1.270 and 2.1.271 builds, the atpkg store build 2.1.316 and the Homebrew
/// 2.1.170, 2026-10-31; `Authority=Developer ID Application: Anthropic PBC (Q6L2SF6YDW)`), and the
/// team the kernel reads for a running store build (`csops` `CS_OPS_TEAMID`, 2026-11-01).
pub const CLAUDE_TEAM: &str = "Q6L2SF6YDW";

/// agy's signing team: `TeamIdentifier=EQHXZ8M8AV` (`codesign -dv` on the atpkg store build,
/// 2026-11-01).
pub const AGY_TEAM: &str = "EQHXZ8M8AV";

/// Muse Code's signing team: `TeamIdentifier=V9WTTPBFK9` (`codesign -dv` on the atpkg store
/// build, 2026-11-01).
pub const MUSE_TEAM: &str = "V9WTTPBFK9";

/// The development seam naming the lab's stand-in agents: a path list (`std::env::split_paths`),
/// each a file whose identity a build with debug assertions confirms as an agent's image, or as a
/// script shell's script ([`Image::script`]). A shipped binary never reads it.
pub const STANDINS_SEAM: &str = "ATERM_DEV_AGENT_STANDINS";

/// The most stand-ins [`STANDINS_SEAM`] names that are read.
#[cfg(any(test, debug_assertions))]
const STANDINS_MAX: usize = 64;

/// A file's identity: its device and inode (on Windows its volume serial and file index), which
/// every path to it shares and no process can give another file (H1's rule: a word confirms by
/// identity, never by a string). Where none can be read, [`Self::of`] and [`Self::of_entry`] say
/// `None` and nothing confirms by identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileId {
    /// The device.
    pub dev: u64,
    /// The inode.
    pub ino: u64,
}

impl FileId {
    /// The identity of the file `path` names, links followed.
    #[must_use]
    pub fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        Self::of_meta(path, &meta)
    }

    /// The identity of the regular file `path` itself is: `None` for a link, a directory or
    /// nothing.
    #[must_use]
    pub fn of_entry(path: &Path) -> Option<Self> {
        let meta = std::fs::symlink_metadata(path)
            .ok()
            .filter(|m| m.file_type().is_file())?;
        Self::of_meta(path, &meta)
    }

    /// The device and inode `meta` (of `path`) carries.
    #[cfg(unix)]
    fn of_meta(_path: &Path, meta: &std::fs::Metadata) -> Option<Self> {
        use std::os::unix::fs::MetadataExt as _;
        Some(Self {
            dev: meta.dev(),
            ino: meta.ino(),
        })
    }

    /// The volume serial and file index of the regular file at `path`, opened
    /// ([`super::session_usage::open_identity`]).
    #[cfg(windows)]
    fn of_meta(path: &Path, _meta: &std::fs::Metadata) -> Option<Self> {
        let file = std::fs::File::open(path).ok()?;
        let meta = file.metadata().ok()?;
        super::session_usage::open_identity(&file, &meta).map(|(dev, ino)| Self { dev, ino })
    }

    /// No file identity is read here: nothing confirms by one.
    #[cfg(not(any(unix, windows)))]
    fn of_meta(_path: &Path, _meta: &std::fs::Metadata) -> Option<Self> {
        None
    }
}

/// What confirmed an agent's image ([`proof`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Proof {
    /// An install by the agent vendor's layout ([`in_vendor_layout`]).
    Layout,
    /// The vendor's signing team, as the kernel holds the running image's signature.
    Team,
    /// A lab stand-in a debug build was told of ([`STANDINS_SEAM`]).
    Seam,
}

impl Proof {
    /// Every proof, in the order [`proof`] tries them.
    pub const ALL: [Self; 3] = [Self::Layout, Self::Team, Self::Seam];

    /// The word a log line says it by.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Layout => "layout",
            Self::Team => "team",
            Self::Seam => "seam",
        }
    }
}

/// What the harness may do in a session whose agent it claims: `status supervisor_act=`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Act {
    /// The agent is confirmed ([`proof`]): the harness answers and types as its policy says.
    On,
    /// The agent is not confirmed: its boxes are read, logged and escalated, and nothing is
    /// typed.
    #[default]
    Observe,
}

impl Act {
    /// Every act, as `status supervisor_act=` may print it (beside `-`, no claim of the host's).
    pub const ALL: [Self; 2] = [Self::On, Self::Observe];

    /// The act a proof (or none) gives.
    #[must_use]
    pub const fn of(proof: Option<Proof>) -> Self {
        match proof {
            Some(_) => Self::On,
            None => Self::Observe,
        }
    }

    /// `status supervisor_act=`'s word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Observe => "observe",
        }
    }

    /// The act `word` names.
    #[must_use]
    pub fn of_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|act| act.word() == word)
    }
}

/// What the kernel keeps for a foreground job's running image.
#[derive(Clone, Copy, Debug)]
pub struct Image<'a> {
    /// The image's path as the kernel names it (macOS `proc_pidpath`, Linux `/proc/<pid>/exe`'s
    /// target): links resolved, never a string the process wrote.
    pub exe: &'a Path,
    /// The identity of the file the image was mapped from; `None` reads it from [`Self::exe`].
    pub file: Option<FileId>,
    /// The signing team of the running image's Developer ID signature, as the kernel validated
    /// it and the running code's certificate check holds it (macOS), or `None`.
    pub team: Option<&'a str>,
    /// For a script shell's image (`atpkg::image::SCRIPT_SHELLS`), the identity of the script it
    /// runs (`argv[1]`, a string the process wrote): it confirms only a lab stand-in
    /// ([`Proof::Seam`]), never an install.
    pub script: Option<FileId>,
}

impl<'a> Image<'a> {
    /// The image at `exe`, its identity read from there, with no team and no script.
    #[must_use]
    pub const fn at(exe: &'a Path) -> Self {
        Self {
            exe,
            file: None,
            team: None,
            script: None,
        }
    }

    fn mapped(&self) -> Option<FileId> {
        self.file.or_else(|| FileId::of(self.exe))
    }
}

/// Where the vendors' layouts are on this machine.
#[derive(Clone, Copy, Debug, Default)]
pub struct Places<'a> {
    /// Claude Code's native installer's versions directory ([`claude_versions`]).
    pub claude_versions: Option<&'a Path>,
    /// The managed atpkg prefix's roots, whose store's builds are registered installs.
    pub roots: Option<&'a ImageRoots>,
}

/// Claude Code's native installer's versions directory under `home`.
#[must_use]
pub fn claude_versions(home: &Path) -> PathBuf {
    home.join(".local/share/claude/versions")
}

/// The signing team that vouches for `agent`'s images, where its vendor signs them.
#[must_use]
pub const fn vendor_team(agent: Program) -> Option<&'static str> {
    match agent {
        Program::Claude => Some(CLAUDE_TEAM),
        Program::Agy => Some(AGY_TEAM),
        Program::Muse => Some(MUSE_TEAM),
        Program::Codex | Program::Grok | Program::Kimi | Program::Generic => None,
    }
}

/// THE ONE PER-AGENT VENDOR-LAYOUT PREDICATE: whether `image` is an install of `agent` by its
/// vendor's layout. Relaunch's admission of a Claude Code image, the codex-alab lane's and the
/// harness's claim all call this one.
///
/// - Claude Code: its native installer's own entry ([`claude_native`]), or an atpkg store build
///   registered as `claude`.
/// - codex-alab: an ALab package ([`super::codex_alab::alab_bytes`]).
/// - agy and Muse Code: an atpkg store build registered as theirs.
/// - Grok Build, Kimi Code and anything else: none (the harness drives none of them).
#[must_use]
pub fn in_vendor_layout(agent: Program, image: &Image<'_>, places: &Places<'_>) -> bool {
    match agent {
        Program::Claude => {
            places
                .claude_versions
                .is_some_and(|versions| claude_native(image, versions))
                || stored(agent, image, places)
        }
        Program::Codex => super::codex_alab::alab_bytes(image.exe),
        Program::Agy | Program::Muse => stored(agent, image, places),
        Program::Grok | Program::Kimi | Program::Generic => false,
    }
}

/// Whether `image` is Claude Code's native install under `versions`: that directory's entry of
/// the image's file name is a version-named regular file (never a link) and the very file the
/// image was mapped from. A link in `versions` to another program never confirms it: the kernel
/// names the link's target, and the entry is a link.
#[must_use]
pub fn claude_native(image: &Image<'_>, versions: &Path) -> bool {
    let Some(name) = image.exe.file_name() else {
        return false;
    };
    if name
        .to_str()
        .and_then(atpkg::vendor_direct::Version::parse)
        .is_none()
    {
        return false;
    }
    let entry = FileId::of_entry(&versions.join(name));
    entry.is_some() && entry == image.mapped()
}

/// Whether `image` is an atpkg store build registered as `agent`'s, the store's own entry being
/// the mapped file.
fn stored(agent: Program, image: &Image<'_>, places: &Places<'_>) -> bool {
    let Some(roots) = places.roots else {
        return false;
    };
    let build = atpkg::image::image_build(Some(roots), image.exe, None);
    let registered = matches!(build.source, ImageSource::Store | ImageSource::StoreRecord)
        && build
            .program
            .as_ref()
            .and_then(|tag| aterm_phase::program_of(tag.as_str()))
            == Some(agent);
    let entry = FileId::of_entry(image.exe);
    registered && entry.is_some() && entry == image.mapped()
}

/// Whether the kernel's signature read names `agent`'s vendor's team.
fn signed_by_vendor(agent: Program, image: &Image<'_>) -> bool {
    vendor_team(agent).is_some_and(|team| image.team == Some(team))
}

/// Whether `image`, or the script a script shell runs, is one of `standins` by identity.
fn standin(image: &Image<'_>, standins: &[FileId]) -> bool {
    let named = |id: Option<FileId>| id.is_some_and(|id| standins.contains(&id));
    named(image.mapped()) || named(image.script)
}

/// What confirms `image` as `agent`'s, or `None`: [`in_vendor_layout`], then [`vendor_team`],
/// then, in a build with debug assertions, the lab's stand-ins ([`STANDINS_SEAM`]).
#[must_use]
pub fn proof(agent: Program, image: &Image<'_>, places: &Places<'_>) -> Option<Proof> {
    proof_among(agent, image, places, &standins())
}

/// [`proof`] with the lab's stand-ins given.
#[must_use]
pub fn proof_among(
    agent: Program,
    image: &Image<'_>,
    places: &Places<'_>,
    standins: &[FileId],
) -> Option<Proof> {
    if in_vendor_layout(agent, image, places) {
        Some(Proof::Layout)
    } else if signed_by_vendor(agent, image) {
        Some(Proof::Team)
    } else if standin(image, standins) {
        Some(Proof::Seam)
    } else {
        None
    }
}

/// The lab's stand-ins [`STANDINS_SEAM`] names, each by its identity (links followed): only in a
/// build with debug assertions.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn standins() -> Vec<FileId> {
    standins_in(aterm_types::dev_seam!(STANDINS_SEAM).as_deref())
}

/// The stand-ins a [`STANDINS_SEAM`] value `list` names: its absolute paths, at most
/// [`STANDINS_MAX`], each by its identity.
#[cfg(any(test, debug_assertions))]
fn standins_in(list: Option<&std::ffi::OsStr>) -> Vec<FileId> {
    let Some(list) = list else {
        return Vec::new();
    };
    std::env::split_paths(list)
        .take(STANDINS_MAX)
        .filter(|path| path.is_absolute())
        .filter_map(|path| FileId::of(&path))
        .collect()
}

/// A shipped build knows no stand-ins.
#[cfg(not(any(test, debug_assertions)))]
#[must_use]
pub fn standins() -> Vec<FileId> {
    Vec::new()
}

#[cfg(test)]
#[path = "agent_image_tests.rs"]
mod tests;
