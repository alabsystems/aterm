// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The record a root step leaves when its apply fails, so setup does not ask the Mac password
//! for it again by itself.
//!
//! Setup's one elevation runs each root step, and a step whose apply cannot succeed (a helper
//! clone the signature check refuses, a job launchd will not run, a developer mode that will
//! not turn on) reads owed again afterwards. Asked for again by itself, it brings the password
//! back each time the person answers it, to no end. The step's root half therefore records each
//! failure in a file of its own beside Spotlight's ([`super::spotlight_scope::REFUSED`]), as the
//! **key** of the inputs it failed with, one line each ([`noted`]):
//!
//! - `<key>`: a failure the step can tell is its own ([`Failure::Definite`]: a definite refusal
//!   of the clone, a published job that served in no round, a tool of Apple's that answered with
//!   a failure). It stands until an apply succeeds.
//! - `no-verdict <since> <boot> <key>`: one root run gave no verdict ([`Failure::NoVerdict`]: a
//!   check that did not answer, a tool that could not start, gave no answer in its budget or was
//!   killed), at root's unix second `<since>` in the boot `<boot>` ([`boot_word`]). That may be
//!   the Mac's passing state, so the line stands for nothing.
//! - `stalled <boot> <key>`: a no-verdict came again at least [`APART`] after the first, in the
//!   same boot. The line stands for `<key>` in that boot alone, so a reboot, which may cure the
//!   state, asks for the step again; a no-verdict that comes again after a reboot is `<key>`.
//!
//! A run nobody asked for does not ask the password for a step whose record stands for the
//! inputs it reads now ([`stands`]); the step still reads owed, and a person's own run (Finish
//! setup, a typed `aterm init`) asks for it and tries it again. Every root run that carries the
//! step tries it too, whatever it ran for, and a success removes the record. A broker that
//! serves removes its own build's lines ([`without_key`]).
//!
//! A key names what the apply consumed, so other inputs are asked for afresh: another build or
//! bundle for the helper ([`broker_key`]), another reading for developer mode
//! ([`developer_mode_key`], which names no build: no build of aterm changes whether Apple's
//! tools succeed, and a key that named one would make every update ask by itself). A record that
//! lists [`KEYS`] plain keys stands for every key: a step that failed for that many builds stands
//! until any root run that carries it succeeds. Pending and stalled lines never count toward
//! that bound, and [`noted`] gives them up for room before a plain key would (the oldest pending
//! line, then a stalled one), so a passing state never fills a record and a superseded build's
//! lines never keep a later build's from being written.
//!
//! Only root's own file counts ([`super::spotlight_scope::listed`]): a missing, unreadable or
//! planted one says nothing, and the step is asked for. A build from before the `no-verdict`
//! and `stalled` lines counts every line toward [`KEYS`] and matches only `<key>`; its keys are
//! its own builds', so it reads such a record as it read records before them (one of
//! [`KEYS`] - 1 plain keys and a pending or stalled line, which [`noted`] writes only there,
//! stands for every key to it, as one more plain key would), and a writer of that build adds
//! its plain keys word for word, which this build counts as they are.

use super::spotlight_scope::{Reads, listed};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The LAN broker's record: root's own file, one line a key ([`broker_key`]).
pub const LAN_BROKER: &str = "/Library/Application Support/aterm/lan-broker.refused";

/// Developer mode's record: root's own file, one line a key ([`developer_mode_key`]).
pub const DEVELOPER_MODE: &str = "/Library/Application Support/aterm/developer-mode.refused";

/// The most lines one record holds; one that lists this many plain keys stands for every key.
pub const KEYS: usize = 8;

/// How long after a no-verdict another one in the same boot stalls the step for that boot: the
/// broker adoption's longest assumed stall (`aterm_uds::lan::adoption::STALL`), a busy Mac's
/// code check that a later look answers.
pub const APART: Duration = Duration::from_secs(5 * 60);

/// How a root apply of a step ended short ([`noted`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The failure is the step's own, one the same inputs would meet again.
    Definite,
    /// The run got no verdict: it may be the Mac's passing state.
    NoVerdict,
}

/// A pending line's first word ([`noted`]).
const PENDING: &str = "no-verdict ";

/// A stalled line's first word ([`noted`]).
const STALLED: &str = "stalled ";

/// One line of a record, as [`noted`] writes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Line<'a> {
    /// `<key>`.
    Key(&'a str),
    /// `no-verdict <since> <boot> <key>`.
    Pending {
        since: u64,
        boot: &'a str,
        key: &'a str,
    },
    /// `stalled <boot> <key>`.
    Stalled { boot: &'a str, key: &'a str },
    /// A line with a pending or stalled line's first word and no such line's form, which
    /// matches no key and is kept as it stands.
    Other,
}

impl<'a> Line<'a> {
    fn parse(text: &'a str) -> Self {
        if let Some(rest) = text.strip_prefix(PENDING) {
            let mut words = rest.splitn(3, ' ');
            return match (words.next(), words.next(), words.next()) {
                (Some(since), Some(boot), Some(key))
                    if !since.is_empty()
                        && since.bytes().all(|b| b.is_ascii_digit())
                        && is_word(boot)
                        && is_key(key) =>
                {
                    since
                        .parse()
                        .map_or(Self::Other, |since| Self::Pending { since, boot, key })
                }
                _ => Self::Other,
            };
        }
        if let Some(rest) = text.strip_prefix(STALLED) {
            return match rest.split_once(' ') {
                Some((boot, key)) if is_word(boot) && is_key(key) => Self::Stalled { boot, key },
                _ => Self::Other,
            };
        }
        Self::Key(text)
    }

    /// The key a pending or stalled line is about.
    const fn noting(self) -> Option<&'a str> {
        match self {
            Self::Pending { key, .. } | Self::Stalled { key, .. } => Some(key),
            Self::Key(_) | Self::Other => None,
        }
    }

    /// The boot a pending or stalled line was written in.
    const fn boot(self) -> Option<&'a str> {
        match self {
            Self::Pending { boot, .. } | Self::Stalled { boot, .. } => Some(boot),
            Self::Key(_) | Self::Other => None,
        }
    }
}

/// Whether `word` is one word of a line: not empty, no whitespace.
fn is_word(word: &str) -> bool {
    !word.is_empty() && !word.chars().any(char::is_whitespace)
}

/// `line` as [`noted`] reads it: [`Line::Other`] where it is not UTF-8.
fn parsed(line: &Path) -> Line<'_> {
    line.to_str().map_or(Line::Other, Line::parse)
}

/// Whether `line` is a plain key: the one kind of line that counts toward [`KEYS`].
fn is_plain(line: &Path) -> bool {
    matches!(parsed(line), Line::Key(_))
}

/// The line of `lines` a record gives up for room ([`noted`]): its oldest pending line, else a
/// stalled one, else one of another form; never a plain key.
fn evictable(lines: &BTreeSet<PathBuf>) -> Option<PathBuf> {
    lines
        .iter()
        .filter_map(|line| match parsed(line) {
            Line::Pending { since, .. } => Some((since, line)),
            _ => None,
        })
        .min()
        .map(|(_, line)| line)
        .or_else(|| {
            lines
                .iter()
                .find(|line| matches!(parsed(line), Line::Stalled { .. }))
        })
        .or_else(|| lines.iter().find(|line| parsed(line) == Line::Other))
        .cloned()
}

/// Whether `key` may stand as a key: one non-empty line that no pending or stalled line's first
/// word begins.
fn is_key(key: &str) -> bool {
    !key.is_empty() && !key.contains('\n') && !key.starts_with(PENDING) && !key.starts_with(STALLED)
}

/// The record's lines once root's run of the step with inputs `key` ended with `failure` at
/// root's unix second `now`, in the boot `boot` ([`boot_word`]; `None` where root could not read
/// it): `None` where the record stays as it is.
///
/// - A definite failure lists `<key>`.
/// - A no-verdict lists `no-verdict <now> <boot> <key>`, which stands for nothing. One that
///   comes [`APART`] or more after this key's pending line of the same boot lists
///   `stalled <boot> <key>` in its place, and one that comes after a pending or stalled line of
///   another boot lists `<key>`: the failure outlived a reboot. One that finds the clock before
///   its pending line dates the line `now` and stalls nothing. Without the boot a no-verdict
///   lists nothing.
/// - A stalled line of another boot is dropped wherever the boot is read, before the record is
///   judged full.
/// - Only plain keys bring the record to [`KEYS`] lines. Where the new line would bring it there
///   or past it while it holds a line of another key that is not a plain key, that line goes
///   first ([`evictable`]: the oldest pending line, else a stalled one, else one of another
///   form). A record of [`KEYS`] - 1 plain keys takes a pending or stalled line as its last. A
///   record that lists [`KEYS`] plain keys, or lists `<key>`, already stands for it.
///
/// `None` also for a key that is no one line or begins as a pending or stalled line does, and
/// a boot that is not one word.
#[must_use]
pub fn noted(
    lines: &BTreeSet<PathBuf>,
    key: &str,
    failure: Failure,
    now: u64,
    boot: Option<&str>,
) -> Option<BTreeSet<PathBuf>> {
    if !is_key(key) || boot.is_some_and(|boot| !is_word(boot)) {
        return None;
    }
    let mut others = BTreeSet::new();
    let mut mine = Vec::new();
    for line in lines {
        let parsed = line.to_str().map_or(Line::Other, Line::parse);
        if parsed.noting() == Some(key) {
            mine.push(parsed);
        } else if !matches!(parsed, Line::Stalled { boot: was, .. }
            if boot.is_some_and(|boot| boot != was))
        {
            others.insert(line.clone());
        }
    }
    if others.contains(Path::new(key))
        || others.iter().filter(|line| is_plain(line)).count() >= KEYS
    {
        return None;
    }
    let line = match failure {
        Failure::Definite => key.to_owned(),
        Failure::NoVerdict => {
            let boot = boot?;
            if mine.iter().any(|line| line.boot() != Some(boot)) {
                key.to_owned()
            } else if mine.iter().any(|line| matches!(line, Line::Stalled { .. })) {
                format!("{STALLED}{boot} {key}")
            } else {
                match mine.iter().find_map(|line| match line {
                    Line::Pending { since, .. } => Some(*since),
                    _ => None,
                }) {
                    Some(since) if since <= now && now - since >= APART.as_secs() => {
                        format!("{STALLED}{boot} {key}")
                    }
                    since => format!(
                        "{PENDING}{} {boot} {key}",
                        since.map_or(now, |since| since.min(now))
                    ),
                }
            }
        }
    };
    while others.len() + 1 > KEYS
        || (others.len() + 1 == KEYS && others.iter().any(|line| !is_plain(line)))
    {
        // Every line left is a plain key: fewer than KEYS of them, so the new line fits.
        let Some(gone) = evictable(&others) else {
            break;
        };
        others.remove(&gone);
    }
    others.insert(PathBuf::from(line));
    (others != *lines).then_some(others)
}

/// Whether the record at `file` stands for the step's inputs as they read now: root's own file
/// lists [`KEYS`] plain keys, or lists `key()`, or `stalled <boot()> <key()>`. Pending and
/// stalled lines never count toward [`KEYS`], whoever wrote the plain keys beside them. The key
/// is read only where a record lists something (`None` where the inputs cannot be read), and the
/// boot ([`boot_word`]) only where a stalled line of the key stands, so a Mac with no record pays
/// nothing for either.
#[must_use]
pub fn stands(
    reads: &dyn Reads,
    file: &str,
    key: impl FnOnce() -> Option<String>,
    boot: impl FnOnce() -> Option<String>,
) -> bool {
    let Ok(lines) = listed(reads, file) else {
        return false;
    };
    if lines.is_empty() {
        return false;
    }
    if lines.iter().filter(|line| is_plain(line)).count() >= KEYS {
        return true;
    }
    let Some(key) = key() else {
        return false;
    };
    let mut boots = Vec::new();
    for line in &lines {
        match line.to_str().map(Line::parse) {
            Some(Line::Key(listed)) if listed == key => return true,
            Some(Line::Stalled { boot, key: listed }) if listed == key => boots.push(boot),
            _ => {}
        }
    }
    !boots.is_empty() && boot().is_some_and(|now| boots.contains(&now.as_str()))
}

/// The record's lines less every line `key` names (`<key>`, and its pending and stalled
/// lines): what a broker that serves the build of `key` leaves ([`broker_key`]). `None` where
/// no line names it.
#[must_use]
pub fn without_key(lines: &BTreeSet<PathBuf>, key: &str) -> Option<BTreeSet<PathBuf>> {
    let kept: BTreeSet<PathBuf> = lines
        .iter()
        .filter(|line| {
            line.to_str().is_none_or(|text| {
                let parsed = Line::parse(text);
                parsed != Line::Key(key) && parsed.noting() != Some(key)
            })
        })
        .cloned()
        .collect();
    (kept.len() != lines.len()).then_some(kept)
}

/// A boot session as a record spells it: `kern.bootsessionuuid`'s 128 bits as 32 lower-case hex
/// digits, the form `aterm_uds::process::boot_id` reads. `text` is that form or the UUID's own
/// (`8-4-4-4-12` hex digits, any case), with surrounding whitespace; `None` for anything else.
#[must_use]
pub fn boot_word(text: &str) -> Option<String> {
    let text = text.trim();
    let digits: String = match text.len() {
        32 => text.to_owned(),
        36 if text
            .char_indices()
            .all(|(at, c)| matches!(at, 8 | 13 | 18 | 23) == (c == '-')) =>
        {
            text.chars().filter(|c| *c != '-').collect()
        }
        _ => return None,
    };
    digits
        .bytes()
        .all(|b| b.is_ascii_hexdigit())
        .then(|| digits.to_ascii_lowercase())
}

/// This boot as a record spells it ([`boot_word`]): `None` where it cannot be read, and in a
/// browser, which has none.
#[must_use]
pub fn boot_here() -> Option<String> {
    #[cfg(not(target_family = "wasm"))]
    {
        aterm_uds::process::boot_id().as_deref().and_then(boot_word)
    }
    #[cfg(target_family = "wasm")]
    {
        None
    }
}

/// The key of a developer mode that reads owed: `off` where the mode is off (`on` false), which
/// no account's password changes on a Mac that refuses it, else the console account `uid`
/// whose authorization is missing.
#[must_use]
pub fn developer_mode_key(on: bool, uid: u32) -> String {
    if on {
        format!("member {uid}")
    } else {
        "off".to_owned()
    }
}

/// How long a change of a record waits for another holder of its lock ([`hold`]): root and the
/// LAN broker each hold it only to read and replace one short file.
pub const LOCK_PATIENCE: Duration = Duration::from_secs(10);

/// The lock every change of the record at `record` is made under ([`hold`]): root's own file
/// beside it, `<record>.lock`. Root's steps (the writer of [`noted`]'s lines and the removal a
/// success makes) and the LAN broker that takes out its own install's lines each read and
/// replace the record only while they hold it, so no change is written over a reading another
/// change has since replaced: a broker's rewrite never brings back a record a success removed.
#[must_use]
pub fn lock_of(record: &Path) -> PathBuf {
    let mut lock = record.as_os_str().to_owned();
    lock.push(".lock");
    PathBuf::from(lock)
}

/// Hold the lock of the record at `record` ([`lock_of`]), waiting at most [`LOCK_PATIENCE`] for
/// another holder; it is let go when the returned lock drops. The record's folder must exist.
///
/// # Errors
/// [`aterm_uds::file_lock::LockFile::take`]'s: `NotFound` where the record has no folder,
/// `WouldBlock` where another holder kept the lock past the patience, and `InvalidData` where
/// the lock's path holds no regular file.
#[cfg(not(target_family = "wasm"))]
pub fn hold(record: &Path) -> std::io::Result<aterm_uds::file_lock::LockFile> {
    aterm_uds::file_lock::LockFile::take(
        &lock_of(record),
        std::time::Instant::now() + LOCK_PATIENCE,
        "another setup step or the LAN helper is changing this record",
    )
}

/// The key of a broker install from the bundle `source`, whose executable's code is `code`: a
/// digest of every CodeDirectory (each slice's CPU type, length and bytes), then the bundle's
/// path. `None` for a path a line cannot hold.
#[cfg(unix)]
#[must_use]
pub fn broker_key(
    source: &Path,
    code: &aterm_uds::lan::install::CodeDirectories,
) -> Option<String> {
    let source = source.to_str().filter(|path| !path.contains('\n'))?;
    let mut digest = aterm_hash::FNV128_OFFSET;
    for (cpu, directory) in code.slices() {
        digest = aterm_hash::fnv1a_128_extend(digest, &cpu.to_be_bytes());
        digest = aterm_hash::fnv1a_128_extend(digest, &(directory.len() as u64).to_be_bytes());
        digest = aterm_hash::fnv1a_128_extend(digest, directory);
    }
    Some(format!("{digest:032x} {source}"))
}

/// The key of a broker install from the bundle this process runs from, on this Mac
/// ([`broker_key`]): `None` outside a bundle, where its code cannot be read, and off macOS.
#[must_use]
pub fn broker_key_here() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        use aterm_uds::lan::install::{BUNDLE_EXECUTABLE, CodeDirectories, source_here};
        let source = source_here()?;
        let code = CodeDirectories::of_file(&source.join(BUNDLE_EXECUTABLE)).ok()?;
        broker_key(&source, &code)
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

#[cfg(test)]
#[path = "refused_tests.rs"]
mod tests;
