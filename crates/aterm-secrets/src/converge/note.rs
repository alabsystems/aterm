// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Why a converge runner was started, as its starter notes it and the runner's aterm.log line
//! carries it. A starter (the vault subscriber's [`super::Due`], the window's lane) keeps the
//! causes of the pass it owes and hands them to the runner in one environment variable
//! ([`START_NOTE_ENV`]) that no other program reads. The note is words and numbers alone: no
//! path, name or address. What
//! moved a starter's repository input mark is in it as classes ([`MarkMoved`]: which kind of
//! Git metadata, or what else), never as the path that moved.

use std::cell::Cell;
use std::ffi::OsStr;

/// The reasons a starter owes a pass: a set, each with one word in the note.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Causes(u32);

impl Causes {
    /// The subscriber started.
    pub const START: Self = Self(1 << 0);
    /// A vault round applied a version.
    pub const APPLIED: Self = Self(1 << 1);
    /// The first healthy vault round after failed ones.
    pub const RECOVERED: Self = Self(1 << 2);
    /// Local member progress, or a look that recovered from an interrupted observation.
    pub const LOCAL: Self = Self(1 << 3);
    /// Automatic work was held and is let go.
    pub const ACTIVATION: Self = Self(1 << 4);
    /// The input observer asked for a repair.
    pub const INPUT_REPAIR: Self = Self(1 << 5);
    /// The input observer asked for a refresh.
    pub const INPUT_REFRESH: Self = Self(1 << 6);
    /// The canonical identity input changed.
    pub const IDENTITY: Self = Self(1 << 7);
    /// This Mac's routes moved.
    pub const ROUTES: Self = Self(1 << 8);
    /// The starter's clock went back.
    pub const CLOCK: Self = Self(1 << 9);
    /// The refresh boundary passed, or there was none yet.
    pub const RECHECK: Self = Self(1 << 10);
    /// The retry after a runner that failed came due.
    pub const RETRY_FAILED: Self = Self(1 << 11);
    /// The retry after a runner that left work unfinished came due.
    pub const RETRY_UNFINISHED: Self = Self(1 << 12);
    /// Identity's own retry came due.
    pub const RETRY_IDENTITY: Self = Self(1 << 13);
    /// The shared repository wave's retry came due.
    pub const RETRY_WAVE: Self = Self(1 << 14);
    /// The window lane's first pass.
    pub const FIRST: Self = Self(1 << 15);
    /// The window lane's recheck timer, after a pass that ran.
    pub const TIMER: Self = Self(1 << 16);
    /// The window's network moved.
    pub const NETWORK: Self = Self(1 << 17);

    /// Each cause's word, by bit.
    const WORDS: [&'static str; 18] = [
        "start",
        "applied",
        "recovered",
        "local",
        "activation",
        "input-repair",
        "input-refresh",
        "identity",
        "routes",
        "clock",
        "recheck",
        "retry-failed",
        "retry-unfinished",
        "retry-identity",
        "retry-wave",
        "first",
        "timer",
        "network",
    ];

    /// Every cause.
    pub const ALL: Self = Self((1 << Self::WORDS.len()) - 1);

    /// This set and `other`'s causes.
    #[must_use]
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether no cause is set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The set's words joined by `+`, in bit order.
    fn words(self) -> String {
        joined(self.0, &Self::WORDS)
    }
}

/// `bits`' words joined by `+`, in bit order.
fn joined(bits: u32, words: &[&str]) -> String {
    words
        .iter()
        .enumerate()
        .filter(|(bit, _)| bits & (1 << bit) != 0)
        .map(|(_, word)| *word)
        .collect::<Vec<_>>()
        .join("+")
}

/// What moved a starter's repository input mark in a look that called for work, as classes:
/// one word for each kind of watched path whose metadata differs between the look's capture
/// and the one before it, and one for each other reason a look calls for work. A class says
/// which kind of Git metadata moved, never which checkout's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MarkMoved(u32);

impl MarkMoved {
    /// A checkout's `.git` entry.
    pub const GIT_DIR: Self = Self(1 << 0);
    /// A Git administrative directory that is not a checkout's own `.git` entry (a
    /// submodule's, or one `commondir` names).
    pub const ADMIN_DIR: Self = Self(1 << 1);
    /// `config`.
    pub const CONFIG: Self = Self(1 << 2);
    /// `config.worktree`.
    pub const CONFIG_WORKTREE: Self = Self(1 << 3);
    /// `HEAD`.
    pub const HEAD: Self = Self(1 << 4);
    /// `index`.
    pub const INDEX: Self = Self(1 << 5);
    /// `packed-refs`.
    pub const PACKED_REFS: Self = Self(1 << 6);
    /// `shallow`.
    pub const SHALLOW: Self = Self(1 << 7);
    /// `commondir` or `gitdir`.
    pub const POINTER: Self = Self(1 << 8);
    /// `refs`.
    pub const REFS_DIR: Self = Self(1 << 9);
    /// `refs/heads`.
    pub const REFS_HEADS_DIR: Self = Self(1 << 10);
    /// The ref `HEAD` names.
    pub const CURRENT_REF: Self = Self(1 << 11);
    /// `objects`, `objects/pack` or `objects/info`.
    pub const OBJECTS_DIR: Self = Self(1 << 12);
    /// One of the 256 loose-object directories.
    pub const OBJECTS_FANOUT: Self = Self(1 << 13);
    /// `objects/info/alternates`.
    pub const ALTERNATES: Self = Self(1 << 14);
    /// A checkout's `.gitmodules`.
    pub const GITMODULES: Self = Self(1 << 15);
    /// A checkout's own folder.
    pub const ROOT: Self = Self(1 << 16);
    /// The owner, mode or kind of a folder on the way to a watched path that is not watched
    /// itself, or the account a capture admits by.
    pub const SHAPE: Self = Self(1 << 17);
    /// A path the later capture watches and the earlier did not.
    pub const ADDED: Self = Self(1 << 18);
    /// A path the earlier capture watched and the later does not.
    pub const REMOVED: Self = Self(1 << 19);
    /// Something other than a checkout: the selection, the helper, Git itself.
    pub const SOURCE: Self = Self(1 << 20);
    /// There was no mark before this look's.
    pub const FIRST: Self = Self(1 << 21);
    /// A hold on the inputs was let go.
    pub const RESUMED: Self = Self(1 << 22);
    /// The owner or mode of a folder above a control path.
    pub const ADMISSION: Self = Self(1 << 23);

    /// Each class's word, by bit.
    const WORDS: [&'static str; 24] = [
        "git-dir",
        "admin-dir",
        "config",
        "config-worktree",
        "head",
        "index",
        "packed-refs",
        "shallow",
        "pointer",
        "refs-dir",
        "refs-heads-dir",
        "current-ref",
        "objects-dir",
        "objects-fanout",
        "alternates",
        "gitmodules",
        "root",
        "shape",
        "added",
        "removed",
        "source",
        "first",
        "resumed",
        "admission",
    ];

    /// Every class.
    pub const ALL: Self = Self((1 << Self::WORDS.len()) - 1);

    /// This set and `other`'s classes.
    #[must_use]
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether no class is set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The set's words joined by `+`, in bit order.
    fn words(self) -> String {
        joined(self.0, &Self::WORDS)
    }
}

/// What a starter knows as it starts a runner: why the pass is owed and what moved its inputs'
/// mark.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StartNote {
    /// Why the pass is owed: every cause noted since the starter's last start.
    pub causes: Causes,
    /// What moved the starter's repository input mark in the looks that called for work since
    /// its last start: the union over [`Self::moves`] looks, so one look's own set only where
    /// that is 1.
    pub moved: MarkMoved,
    /// How many looks [`Self::moved`] is the union of.
    pub moves: u32,
}

/// The longest note a runner accepts ([`note_word`]).
pub const NOTE_MAX: usize = 240;

impl StartNote {
    /// The note as the runner's line carries it: `c=<cause>+<cause>,m=<class>+<class>,n=<looks>`,
    /// a field left out where it is empty or 0, so a note with nothing to say is empty. Where the
    /// whole would pass [`NOTE_MAX`] bytes its classes are cut, to `m=many`: every cause with the
    /// number at its largest is then 195 bytes, so nothing else is ever cut.
    #[must_use]
    pub fn word(&self) -> String {
        let causes = self.causes.words();
        let whole = self.rendered(&causes, &self.moved.words());
        if whole.len() <= NOTE_MAX {
            whole
        } else {
            self.rendered(&causes, "many")
        }
    }

    fn rendered(&self, causes: &str, moved: &str) -> String {
        let mut fields = Vec::new();
        if !causes.is_empty() {
            fields.push(format!("c={causes}"));
        }
        if !moved.is_empty() {
            fields.push(format!("m={moved}"));
        }
        if self.moves != 0 {
            fields.push(format!("n={}", self.moves));
        }
        fields.join(",")
    }
}

/// The variable a starter sets on its runner to the start's note ([`StartNote::word`]). The
/// runner writes it into its aterm.log line once [`note_word`] admits it; nothing else reads it.
pub const START_NOTE_ENV: &str = "ATERM_CONVERGE_START";

thread_local! {
    /// The note of the start this thread is making ([`noting`]).
    static NOTE: Cell<Option<StartNote>> = const { Cell::new(None) };
}

/// Run `start` with `note` as this thread's start note: a runner command built inside it
/// ([`super::runner`], [`super::runner_requested`]) carries the note in [`START_NOTE_ENV`]. The
/// note is this thread's for the call alone, also where `start` unwinds.
pub fn noting<T>(note: &StartNote, start: impl FnOnce() -> T) -> T {
    struct Restore(Option<StartNote>);
    impl Drop for Restore {
        fn drop(&mut self) {
            NOTE.set(self.0);
        }
    }
    let _restore = Restore(NOTE.replace(Some(*note)));
    start()
}

/// The word of the note this thread is starting a runner under, where there is one and it says
/// anything.
pub(super) fn noted() -> Option<String> {
    NOTE.get()
        .map(|note| note.word())
        .filter(|word| !word.is_empty())
}

/// `value` as a start note a runner writes into its log line: the value where it is at most
/// [`NOTE_MAX`] bytes, not empty, and made of `a-z`, `0-9`, `+`, `,`, `=` and `-` alone, so no
/// space, bracket or name reaches the line from the environment.
#[must_use]
pub fn note_word(value: &OsStr) -> Option<String> {
    let word = value.to_str()?;
    let plain = |b: u8| {
        b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'+' | b',' | b'=' | b'-')
    };
    (!word.is_empty() && word.len() <= NOTE_MAX && word.bytes().all(plain)).then(|| word.to_owned())
}

#[cfg(test)]
#[path = "note_tests.rs"]
mod tests;
