// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A VALUE SOURCE'S EPISODES AND THEIR DWELL (`docs/DESIGN-harness-occupancy-2026-10-20.md` §3.1
//! "Episodes"; the slice-3 plan's D7): what a flap inside the dwell is, made exact.
//!
//! A read whose value differs from the last good read's is an EDGE. An edge with no episode
//! open opens one, its base the last read's value. An edge while the open one has not dwelt
//! joins it (`moves + 1`, `latest` the new value); once it has dwelt it is superseded and a new
//! one opens, its base the old one's `latest`. An episode has DWELT once [`DWELL_READS`] good
//! reads in a row saw no edge and the last of them came at least the dwell after its last edge,
//! so `every = 1h` dwells after two reads, 2 h. It closes, never told, when the source reads its
//! base again for the dwell (the move was undone); otherwise it never expires by time: a session
//! whose next break comes hours later is still told, with the move's own time.
//!
//! An unread read ([`ReadWord`]) is neither an edge nor a quiet read: it changes nothing.
//!
//! The book persists after every read that changed it (the board's sources.json, one atomic
//! write: [`PersistedBook`]). [`SourceBook::restore`] keeps what it saved, so a move made while
//! aterm was down is an edge at the first read; a book whose last good read is older than
//! [`STALE_MS`] re-bases (its first read is a base again, journaled `rebased why=stale`), and so
//! does one kept for another target ([`target_of`]: the source was pointed at another remote,
//! ref, path or program, `rebased why=repointed`), so the first read of the new target is never
//! an edge from the old one's value. The file is advisory: an entry for a source no table
//! declares is dropped, a stamp past now is clamped to now, and a file over [`SOURCES_FILE_MAX`]
//! is not read.
//!
//! Pure and clockless: every instant is the caller's.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::owner_rules::{Checked, OwnerName, Resolved, Source, SourceKind};

const MINUTE_MS: i64 = 60_000;
const HOUR_MS: i64 = 60 * MINUTE_MS;

/// The dwell: no edge for this long, over [`DWELL_READS`] reads at least.
pub const DWELL_MS: i64 = 10 * MINUTE_MS;

/// The good reads in a row without an edge a dwell needs.
pub const DWELL_READS: u32 = 2;

/// A book whose last good read is older than this re-bases at its restore.
pub const STALE_MS: i64 = 24 * HOUR_MS;

/// The most bytes of sources.json read.
pub const SOURCES_FILE_MAX: usize = 64 * 1024;

/// The longest atpkg build value.
const BUILD_MAX: usize = 64;

/// WHAT ONE GOOD READ OF A SOURCE FOUND: a git object id (a ref's tip, or the tree entry under
/// it), or the build an atpkg index pins. Nothing else a remote or an index says is kept.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SourceValue {
    /// 40 or 64 lower-case hex digits.
    Sha(String),
    /// `[A-Za-z0-9._+-]{1,64}`.
    Build(String),
}

impl SourceValue {
    /// The object id `text` spells, if it is one.
    #[must_use]
    pub fn sha(text: &str) -> Option<Self> {
        let hex = |b: u8| matches!(b, b'0'..=b'9' | b'a'..=b'f');
        (matches!(text.len(), 40 | 64) && text.bytes().all(hex)).then(|| Self::Sha(text.into()))
    }

    /// The build `text` spells, if it is one.
    #[must_use]
    pub fn build(text: &str) -> Option<Self> {
        let ok = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-');
        ((1..=BUILD_MAX).contains(&text.len()) && text.bytes().all(ok))
            .then(|| Self::Build(text.into()))
    }

    /// As a line names it: an id's first 10 digits, a build whole.
    #[must_use]
    pub fn short(&self) -> &str {
        match self {
            Self::Sha(hex) => &hex[..10],
            Self::Build(build) => build,
        }
    }

    /// Its stored spelling: `sha:<hex>` or `build:<text>`.
    #[must_use]
    pub fn spelled(&self) -> String {
        match self {
            Self::Sha(hex) => format!("sha:{hex}"),
            Self::Build(build) => format!("build:{build}"),
        }
    }

    fn of_spelling(text: &str) -> Option<Self> {
        match text.split_once(':')? {
            ("sha", hex) => Self::sha(hex),
            ("build", build) => Self::build(build),
            _ => None,
        }
    }
}

impl Serialize for SourceValue {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.spelled())
    }
}

impl<'de> Deserialize<'de> for SourceValue {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        Self::of_spelling(&text)
            .ok_or_else(|| serde::de::Error::custom("a value is sha:<hex> or build:<text>"))
    }
}

/// WHY A READ FOUND NO VALUE: a closed word, never git's stderr or a URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReadWord {
    /// The host did not answer, or no route to it.
    Unreachable,
    /// The remote asked for a credential aterm does not give.
    Auth,
    /// The host's key is not one the owner's known hosts hold.
    HostKey,
    /// The one-minute hang detector cut the read.
    Timeout,
    /// The source is refused before any read (its path, its URL, its socket, its output).
    Refused,
    /// No developer git on this Mac.
    NoGit,
    /// The remote has no such ref, or the tree no such path.
    NoRef,
    /// What came back is not one aterm reads.
    Unread,
}

impl ReadWord {
    /// Every word.
    pub const ALL: [ReadWord; 8] = [
        ReadWord::Unreachable,
        ReadWord::Auth,
        ReadWord::HostKey,
        ReadWord::Timeout,
        ReadWord::Refused,
        ReadWord::NoGit,
        ReadWord::NoRef,
        ReadWord::Unread,
    ];

    /// Its word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            ReadWord::Unreachable => "unreachable",
            ReadWord::Auth => "auth",
            ReadWord::HostKey => "host-key",
            ReadWord::Timeout => "timeout",
            ReadWord::Refused => "refused",
            ReadWord::NoGit => "no-git",
            ReadWord::NoRef => "no-ref",
            ReadWord::Unread => "unread",
        }
    }
}

/// ONE EPISODE: the source moved off its base and has not been undone for the dwell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Episode {
    /// Its id: FNV-1a over the source's name, its base, its first new value and its opening.
    pub id: u64,
    /// The value before its first edge.
    pub base: SourceValue,
    /// The value its last edge read.
    pub latest: SourceValue,
    /// Its edges, 1 at its opening.
    pub moves: u32,
    /// Its first edge, ms.
    pub opened_ms: i64,
    /// Its last edge, ms.
    pub last_edge_ms: i64,
}

/// What an edge did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    /// Opened an episode.
    Opened,
    /// Joined the open one, inside its dwell.
    Joined,
    /// Superseded the open one, past its dwell, and opened another.
    Superseded,
}

/// WHAT ONE READ DID ([`SourceBook::look`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The first good read (or the first after a re-base): the base, no edge.
    Based,
    /// A new value.
    Edge(Edge),
    /// The value the last read saw.
    Quiet,
    /// The value the last read saw, the episode's base, for the dwell: the move was undone and
    /// the episode closes, never told.
    Closed,
    /// No value: nothing changes.
    Unread(ReadWord),
}

/// What a restore did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Restored {
    /// The book as saved.
    Kept,
    /// Its last good read was older than [`STALE_MS`]: the next read is a base.
    Rebased,
    /// It was kept for another target ([`target_of`]): the next read is a base.
    Repointed,
}

/// WHAT A SOURCE'S READS READ, the target its book is kept for: FNV-1a over its kind and, for a
/// git source, its remote's stored URL, its ref and its path, for an atpkg source its program.
/// Its name and its `every` are no part of it. A git source whose remote is not resolved yet
/// has none: nothing reads it.
#[must_use]
pub fn target_of(source: &Source, resolved: &Resolved) -> Option<u64> {
    let form = match &source.kind {
        SourceKind::Git(git) => format!(
            "git-ref url={:?} ref={:?} path={:?}",
            source.remote(resolved)?.stored(),
            git.git_ref.as_str(),
            git.path.as_ref().map(Checked::as_str)
        ),
        SourceKind::Atpkg(atpkg) => format!("atpkg program={:?}", atpkg.program.as_str()),
    };
    Some(aterm_hash::fnv1a_64(format!("target {form}").as_bytes()))
}

/// ONE SOURCE'S BOOK ([module docs](self)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBook {
    name: OwnerName,
    saved: PersistedBook,
}

/// A BOOK AS sources.json HOLDS IT.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedBook {
    /// The target it is kept for ([`target_of`]).
    #[serde(default)]
    pub target: Option<u64>,
    /// The last good read's value.
    #[serde(default)]
    pub seen: Option<SourceValue>,
    /// The open episode.
    #[serde(default)]
    pub episode: Option<Episode>,
    /// Good reads in a row without an edge, at most [`DWELL_READS`].
    #[serde(default)]
    pub quiet: u32,
    /// The last of them, ms.
    #[serde(default)]
    pub quiet_ms: i64,
    /// The last good read, ms.
    #[serde(default)]
    pub read_ms: Option<i64>,
}

impl SourceBook {
    /// A book that has read nothing, kept for `target` ([`target_of`]).
    #[must_use]
    pub fn new(name: OwnerName, target: u64) -> Self {
        Self {
            name,
            saved: PersistedBook {
                target: Some(target),
                ..PersistedBook::default()
            },
        }
    }

    /// The source's name.
    #[must_use]
    pub const fn name(&self) -> &OwnerName {
        &self.name
    }

    /// The open episode.
    #[must_use]
    pub const fn episode(&self) -> Option<&Episode> {
        self.saved.episode.as_ref()
    }

    /// The last good read's value.
    #[must_use]
    pub const fn seen(&self) -> Option<&SourceValue> {
        self.saved.seen.as_ref()
    }

    /// The book as sources.json holds it.
    #[must_use]
    pub const fn persisted(&self) -> &PersistedBook {
        &self.saved
    }

    /// Whether the open episode has dwelt: [`DWELL_READS`] quiet reads, the last at least
    /// `dwell_ms` after its last edge.
    #[must_use]
    pub fn dwelt(&self, dwell_ms: i64) -> bool {
        use super::fault::{Fault, armed};
        let Some(episode) = &self.saved.episode else {
            return false;
        };
        if armed(Fault::DwellCloseEarly) {
            return self.saved.quiet >= 1;
        }
        self.saved.quiet >= DWELL_READS
            && self.saved.quiet_ms.saturating_sub(episode.last_edge_ms) >= dwell_ms
    }

    /// ONE READ at `at_ms` ([module docs](self)). An instant before the last good read is
    /// taken as that read's.
    pub fn look(
        &mut self,
        value: Result<SourceValue, ReadWord>,
        at_ms: i64,
        dwell_ms: i64,
    ) -> Step {
        use super::fault::{Fault, armed};
        let value = match value {
            Ok(value) => value,
            Err(word) => return Step::Unread(word),
        };
        let at_ms = self.saved.read_ms.map_or(at_ms, |last| at_ms.max(last));
        let dwelt = self.dwelt(dwell_ms);
        self.saved.read_ms = Some(at_ms);
        let Some(seen) = self.saved.seen.replace(value.clone()) else {
            self.saved.quiet = 0;
            return Step::Based;
        };
        if seen == value {
            self.saved.quiet = if armed(Fault::QuietRestarts) {
                0
            } else {
                self.saved.quiet.saturating_add(1).min(DWELL_READS)
            };
            self.saved.quiet_ms = at_ms;
            let undone = self.saved.episode.as_ref().is_some_and(|e| e.base == value);
            if undone && self.dwelt(dwell_ms) {
                self.saved.episode = None;
                return Step::Closed;
            }
            return Step::Quiet;
        }
        self.saved.quiet = 0;
        let edge = match self.saved.episode.as_mut() {
            Some(open) if !dwelt => {
                open.latest = value;
                open.moves = open.moves.saturating_add(1);
                open.last_edge_ms = at_ms;
                return Step::Edge(Edge::Joined);
            }
            Some(_) => Edge::Superseded,
            None => Edge::Opened,
        };
        self.saved.episode = Some(Episode {
            id: episode_id(&self.name, &seen, &value, at_ms),
            base: seen,
            latest: value,
            moves: 1,
            opened_ms: at_ms,
            last_edge_ms: at_ms,
        });
        Step::Edge(edge)
    }

    /// THE BOOK sources.json SAVED, AS OF `now_ms`, for the source's `target` now: every stamp
    /// past now clamped to now, an episode that cannot be one dropped, and re-based when it was
    /// kept for another target or its last good read is older than [`STALE_MS`]
    /// ([module docs](self)).
    #[must_use]
    pub fn restore(
        name: OwnerName,
        target: u64,
        persisted: PersistedBook,
        now_ms: i64,
    ) -> (Self, Restored) {
        use super::fault::{Fault, armed};
        if persisted.target != Some(target) {
            return (Self::new(name, target), Restored::Repointed);
        }
        let mut saved = persisted;
        let clamp = |ms: &mut i64| *ms = (*ms).min(now_ms);
        if let Some(read_ms) = saved.read_ms.as_mut() {
            clamp(read_ms);
        }
        clamp(&mut saved.quiet_ms);
        saved.quiet = saved.quiet.min(DWELL_READS);
        if let Some(e) = saved.episode.as_mut() {
            clamp(&mut e.opened_ms);
            clamp(&mut e.last_edge_ms);
        }
        let sound = |e: &Episode| {
            e.moves >= 1 && e.opened_ms <= e.last_edge_ms && saved.seen.as_ref() == Some(&e.latest)
        };
        if saved.episode.as_ref().is_some_and(|e| !sound(e)) {
            saved.episode = None;
        }
        let stale = match saved.read_ms {
            None => false,
            Some(read_ms) => now_ms.saturating_sub(read_ms) > STALE_MS,
        };
        if stale || (armed(Fault::RebaseOnRestart) && saved.read_ms.is_some()) {
            return (Self::new(name, target), Restored::Rebased);
        }
        (Self { name, saved }, Restored::Kept)
    }
}

fn episode_id(name: &OwnerName, base: &SourceValue, first: &SourceValue, opened_ms: i64) -> u64 {
    aterm_hash::fnv1a_64(
        format!(
            "source {} {} {} {opened_ms}",
            name.as_str(),
            base.spelled(),
            first.spelled()
        )
        .as_bytes(),
    )
}

/// AN OWNER RULE'S EPISODE KEY (F5): FNV-1a over the rule's name and its source's episode id,
/// so two rules on one source each book their own episode.
#[must_use]
pub fn owner_episode_key(rule: &OwnerName, source_episode: u64) -> u64 {
    aterm_hash::fnv1a_64(format!("owner {} {source_episode:016x}", rule.as_str()).as_bytes())
}

/// sources.json, as the board writes it and reads it back.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourcesFile {
    /// Each source's book, by name.
    #[serde(default)]
    pub books: BTreeMap<String, PersistedBook>,
}

/// Why sources.json is not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourcesFileRefusal {
    /// Over [`SOURCES_FILE_MAX`] bytes.
    Long,
    /// Not the file's JSON.
    Unreadable,
}

impl SourcesFile {
    /// The file of `books`.
    #[must_use]
    pub fn of<'a>(books: impl IntoIterator<Item = &'a SourceBook>) -> Self {
        Self {
            books: books
                .into_iter()
                .map(|b| (b.name.as_str().to_string(), b.saved.clone()))
                .collect(),
        }
    }

    /// Its JSON.
    #[must_use]
    pub fn to_json(&self) -> String {
        aterm_json::to_string(self).unwrap_or_default()
    }

    /// THE BOOKS `bytes` HOLD for the sources `declared` (each by its name and its target now),
    /// as of `now_ms` (each [`SourceBook::restore`]d), an entry for any other name dropped.
    /// # Errors
    /// [`SourcesFileRefusal`]: a file too long, or not its JSON.
    pub fn read(
        bytes: &[u8],
        declared: &[(OwnerName, u64)],
        now_ms: i64,
    ) -> Result<Vec<(SourceBook, Restored)>, SourcesFileRefusal> {
        if bytes.len() > SOURCES_FILE_MAX {
            return Err(SourcesFileRefusal::Long);
        }
        let file: Self =
            aterm_json::from_slice(bytes).map_err(|_| SourcesFileRefusal::Unreadable)?;
        Ok(file
            .books
            .into_iter()
            .filter_map(|(written, book)| {
                let (name, target) = declared.iter().find(|(n, _)| n.as_str() == written)?;
                Some(SourceBook::restore(name.clone(), *target, book, now_ms))
            })
            .collect())
    }
}

#[cfg(test)]
#[path = "source_episode_tests.rs"]
mod tests;
