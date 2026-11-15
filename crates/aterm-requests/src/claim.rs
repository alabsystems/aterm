// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A request's footprint outside its tabs (the requests design's §7 and step 4 of its build
//! order): the folders it claims, and the pure judges a claim is made and kept by.
//!
//! A folder becomes a request's three ways ([`ClaimSrc`]): it is the folder a request's spawn
//! opened in and its birth falls inside the request's life (`cwd`), it was born directly under
//! `$HOME` or a request's root while a request tab's command ran (`born`), or the requester named
//! it (`named`, `aterm ctl request claim path=…`). Nothing here deletes: [`keep_of`] says which
//! claims a later cleanup must keep, and why.

use crate::store::Request;
use std::path::{Component, Path, PathBuf};

/// The most bytes a claimed path takes percent-encoded: the session log's `cwd` cap. A longer
/// path is refused, never clamped.
pub const CLAIM_PATH_BYTES: usize = 256;

/// The most folders one request claims.
pub const MAX_CLAIMS: usize = 64;

/// The most roots (folders a request's spawns opened in) one request records.
pub const MAX_ROOTS: usize = 8;

/// How far a folder's birth may lie outside a command's span and still count as the command's:
/// a clock read on either side, and a block's start stamped after its first write.
pub const BIRTH_SKEW_MS: u64 = 2_000;

/// The most entries a birth scan lists under one root in one look.
pub const HOME_CHILD_MAX: usize = 4096;

/// How long the requests' writer keeps the command spans it was handed.
pub const SPAN_RING_MS: u64 = 10 * 60_000;

/// The longest a request's command may run and still say alone that a folder born while it ran
/// is the request's ([`alone`]): a longer one (an agent's session that runs for hours) holds every
/// folder born in that time, and a person makes folders with no tab's command (Finder, an editor,
/// another app) that no span shows.
pub const ALONE_SPAN_MS: u64 = 15 * 60_000;

/// How a folder became a request's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimSrc {
    /// The folder a spawn of it opened in, born inside its life.
    Cwd,
    /// A folder born under `$HOME` or a root while one of its tabs' commands ran.
    Born,
    /// Its requester named it.
    Named,
}

impl ClaimSrc {
    /// Its `src=` word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Cwd => "cwd",
            Self::Born => "born",
            Self::Named => "named",
        }
    }

    pub(crate) fn of(word: &str) -> Option<Self> {
        [Self::Cwd, Self::Born, Self::Named]
            .into_iter()
            .find(|src| src.word() == word)
    }
}

/// Why a command in a claimed folder counts as a person's touch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchWhy {
    /// It ran in a tab no open request owns.
    PersonTab,
    /// A person typed in its request tab while it was entered.
    Typed,
    /// It ran in a request tab a person kept.
    Kept,
}

impl TouchWhy {
    /// Its `why=` word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::PersonTab => "person-tab",
            Self::Typed => "typed",
            Self::Kept => "kept",
        }
    }

    pub(crate) fn of(word: &str) -> Option<Self> {
        [Self::PersonTab, Self::Typed, Self::Kept]
            .into_iter()
            .find(|why| why.word() == word)
    }
}

/// One claim as a request's `claim` record keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    /// The folder, absolute, in [`normal`] form.
    pub path: String,
    /// How it became the request's.
    pub src: ClaimSrc,
    /// Who claimed it: the requester for a `named` claim; the request's requester for one aterm
    /// found.
    pub by: String,
    /// The request tab whose command made it, or whose caller named it.
    pub tab: Option<String>,
    /// When it was born, Unix milliseconds, where the filesystem says.
    pub born_ms: Option<u64>,
    /// Whether its birth is its request's alone ([`alone`]): `Some(false)` where another
    /// request's or a person's tab ran a command then, `None` where no bounded command of its
    /// request's accounts for it (its birth unread, or outside every span the writer holds).
    pub alone: Option<bool>,
}

/// A claim as its request holds it: the record and when it was written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldClaim {
    /// The claim.
    pub claim: Claim,
    /// When it was recorded, Unix milliseconds.
    pub at_ms: u64,
}

/// What a measure of a claimed folder found (never journaled: a later look measures again).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Measured {
    /// When, Unix milliseconds.
    pub at_ms: u64,
    /// Its size, bytes: at least this much where [`Self::complete`] is false; `None` where the
    /// walk could not read it at all.
    pub bytes: Option<u64>,
    /// Whether the walk saw all of it.
    pub complete: bool,
    /// Whether a git repo in it holds work no remote has (`Some(false)` where every repo found
    /// is pushed, or none was found); `None` where a repo could not be read.
    pub unpushed: Option<bool>,
}

/// Why a cleanup must keep a claimed folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keep {
    /// It was born before its request opened, or after it ended, or its birth is unread.
    BornBefore,
    /// Another tab's command ran when it was born.
    Contested,
    /// No bounded command of its request's alone accounts for its birth ([`alone`]).
    Unattributed,
    /// A person worked in it after it was claimed.
    Touched,
    /// A repo in it holds work no remote has.
    Unpushed,
    /// It has not been measured, or a repo in it could not be read.
    Unmeasured,
    /// Its request is kept (`request keep`): its folders are a person's.
    Kept,
    /// The folder at its path now is not the one claimed: its birth moved.
    Replaced,
    /// A tab that still stands works in it, or in a folder inside it.
    InUse,
    /// The cleanup's manifest of it is not whole (a cap, a folder it could not read).
    Unlisted,
    /// The cleanup's archive was not written.
    Unarchived,
    /// It changed after the cleanup listed it, so its listing no longer describes it, or it could
    /// not be listed again whole.
    Changed,
    /// It was not there when a first cleanup looked: something else removed or moved it.
    Missing,
}

impl Keep {
    /// Its word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::BornBefore => "born-before",
            Self::Contested => "contested",
            Self::Unattributed => "unattributed",
            Self::Touched => "touched",
            Self::Unpushed => "unpushed",
            Self::Unmeasured => "unmeasured",
            Self::Kept => "kept",
            Self::Replaced => "replaced",
            Self::InUse => "in-use",
            Self::Unlisted => "unlisted",
            Self::Unarchived => "unarchived",
            Self::Changed => "changed",
            Self::Missing => "missing",
        }
    }

    /// Every reason.
    pub const ALL: [Self; 13] = [
        Self::BornBefore,
        Self::Contested,
        Self::Unattributed,
        Self::Touched,
        Self::Unpushed,
        Self::Unmeasured,
        Self::Kept,
        Self::Replaced,
        Self::InUse,
        Self::Unlisted,
        Self::Unarchived,
        Self::Changed,
        Self::Missing,
    ];

    /// The reason a word names.
    #[must_use]
    pub fn of(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|keep| keep.word() == word)
    }
}

/// `path` in the one form a claim records: absolute, every `.` dropped, no trailing separator.
///
/// # Errors
/// Why it cannot be recorded: it is relative, names a `..`, is the root folder, or encodes past
/// [`CLAIM_PATH_BYTES`].
pub fn normal(path: &str) -> Result<String, &'static str> {
    let given = Path::new(path);
    if !given.is_absolute() {
        return Err("is not an absolute path");
    }
    let mut out = PathBuf::new();
    for part in given.components() {
        match part {
            Component::ParentDir => return Err("names a `..`"),
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    if out.parent().is_none() {
        return Err("is the root folder");
    }
    let out = out
        .to_str()
        .ok_or("is not UTF-8")?
        .trim_end_matches(std::path::MAIN_SEPARATOR)
        .to_owned();
    if aterm_codec::pct::encoded_len(&out) > CLAIM_PATH_BYTES {
        return Err("is too long to record");
    }
    Ok(out)
}

/// One command's span in a tab: from its input to its end, `None` while it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    /// When it was entered, Unix milliseconds.
    pub from_ms: u64,
    /// When it ended, Unix milliseconds; `None` while it runs.
    pub to_ms: Option<u64>,
}

impl Span {
    /// Whether a folder born at `born_ms` was born while this ran, give or take `skew`.
    #[must_use]
    pub fn holds(&self, born_ms: u64, skew: u64) -> bool {
        born_ms >= self.from_ms.saturating_sub(skew)
            && self
                .to_ms
                .is_none_or(|to| born_ms <= to.saturating_add(skew))
    }
}

/// Whether a folder born at `born_ms` is a request's whose commands ran in `spans`: born while
/// one ran ([`Span::holds`], give or take `skew`), and not after the request ended (`ended_ms`).
#[must_use]
pub fn born_in(born_ms: u64, spans: &[Span], skew: u64, ended_ms: Option<u64>) -> bool {
    ended_ms.is_none_or(|end| born_ms <= end.saturating_add(skew))
        && spans.iter().any(|span| span.holds(born_ms, skew))
}

/// Whether a folder born at `born_ms` is alone the request's whose commands ran in `mine`, with
/// every other request's and person's tab's in `others` (each give or take [`BIRTH_SKEW_MS`]):
/// `Some(false)` where one of `others` ran then; `Some(true)` where one of `mine` ran then and
/// each of `mine` that did ended within [`ALONE_SPAN_MS`] of its start; else `None`, unknown (no
/// span of `mine` holds it, or one that does still runs or ran longer), which a cleanup keeps.
#[must_use]
pub fn alone(born_ms: u64, mine: &[Span], others: &[Span]) -> Option<bool> {
    if others.iter().any(|span| span.holds(born_ms, BIRTH_SKEW_MS)) {
        return Some(false);
    }
    let mut held = mine
        .iter()
        .filter(|span| span.holds(born_ms, BIRTH_SKEW_MS))
        .peekable();
    (held.peek().is_some()
        && held.all(|span| {
            span.to_ms
                .is_some_and(|to| to.saturating_sub(span.from_ms) <= ALONE_SPAN_MS)
        }))
    .then_some(true)
}

/// The claim of `claims` that `path` lies at or under, component by component (`/a/foo` is not
/// under `/a/fo`): the deepest, where claims nest (a store never records nested claims).
#[must_use]
pub fn inside<'a>(path: &str, claims: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let path = Path::new(path);
    claims
        .into_iter()
        .filter(|claim| path.starts_with(Path::new(claim)))
        .max_by_key(|claim| Path::new(claim).components().count())
}

/// A claim of `claims` that lies under `path`, itself excluded: a folder holding a claim is never
/// claimed, since a cleanup of it would take the claim's folder with it.
#[must_use]
pub fn within<'a>(path: &str, claims: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let path = Path::new(path);
    claims.into_iter().find(|claim| {
        let claim = Path::new(claim);
        claim != path && claim.starts_with(path)
    })
}

/// Why a cleanup must keep `claim` of `request`, measured as `measured`, at `now_ms`; `None`
/// where it may delete it (after archiving it). Pure: it writes nothing.
///
/// A folder is kept where its birth is unread or outside the request's life (opened, less
/// [`BIRTH_SKEW_MS`], to its end or now), another tab ran a command at its birth, no bounded
/// command of its request's alone accounts for its birth, a person worked in it, or a repo in it
/// holds unpushed work or could not be read.
#[must_use]
pub fn keep_of(
    request: &Request,
    claim: &Claim,
    measured: Option<&Measured>,
    now_ms: u64,
) -> Option<Keep> {
    let until = request.ended_ms.unwrap_or(now_ms);
    let born_inside = claim.born_ms.is_some_and(|born| {
        born >= request.opened_ms.saturating_sub(BIRTH_SKEW_MS)
            && born <= until.saturating_add(BIRTH_SKEW_MS)
    });
    if !born_inside {
        return Some(Keep::BornBefore);
    }
    match claim.alone {
        Some(false) => return Some(Keep::Contested),
        None => return Some(Keep::Unattributed),
        Some(true) => {}
    }
    if request.touched.contains(&claim.path) {
        return Some(Keep::Touched);
    }
    match measured.and_then(|measured| measured.unpushed) {
        None => Some(Keep::Unmeasured),
        Some(true) => Some(Keep::Unpushed),
        Some(false) => None,
    }
}

/// What a cleanup found of a claimed folder on the host, as [`clean_verdict`] judges it.
#[derive(Clone, Copy, Debug)]
pub struct HostFacts<'a> {
    /// When the cleanup began (its `clean` record), Unix milliseconds: a measure older than this
    /// is no measure of the folder as the cleanup found it.
    pub clean_at_ms: u64,
    /// The birth of the folder at the claim's path now, where the filesystem says.
    pub born_now: Option<u64>,
    /// The working folder of every tab that still stands (in [`normal`] form).
    pub cwds: &'a [String],
    /// Whether the cleanup's manifest of the folder is whole.
    pub listed: bool,
    /// Whether the cleanup's archive was written.
    pub archived: bool,
}

/// Why a cleanup must keep `held` of `request`, measured as `measured`, with the host as `host`
/// says, at `now_ms`; `None` where it may delete it. Pure: it writes nothing.
///
/// In order: the request is kept; [`keep_of`] (with a measure the cleanup did not make after it
/// began read as none, so [`Keep::Unmeasured`]); the folder's birth moved; a standing tab works at
/// or under it; its manifest is not whole; the archive was not written.
#[must_use]
pub fn clean_verdict(
    request: &Request,
    held: &HeldClaim,
    measured: Option<&Measured>,
    host: &HostFacts<'_>,
    now_ms: u64,
) -> Option<Keep> {
    if request.kept.is_some() {
        return Some(Keep::Kept);
    }
    let fresh = measured.filter(|measured| measured.at_ms >= host.clean_at_ms);
    if let Some(keep) = keep_of(request, &held.claim, fresh, now_ms) {
        return Some(keep);
    }
    if host.born_now.is_none() || host.born_now != held.claim.born_ms {
        return Some(Keep::Replaced);
    }
    if inside_any(&held.claim.path, host.cwds) {
        return Some(Keep::InUse);
    }
    if !host.listed {
        return Some(Keep::Unlisted);
    }
    if !host.archived {
        return Some(Keep::Unarchived);
    }
    None
}

/// Whether any of `cwds` lies at or under `path`, component by component.
fn inside_any(path: &str, cwds: &[String]) -> bool {
    let path = Path::new(path);
    cwds.iter().any(|cwd| Path::new(cwd).starts_with(path))
}

#[cfg(test)]
#[path = "claim_tests.rs"]
mod tests;
