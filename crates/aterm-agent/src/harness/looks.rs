// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT A FOOTER READ LOOKED AT BEFORE IT READ: each file the Claude Code footer's read takes a
//! fact from ([`Input`]), with how a watch on it should hear it ([`Heed`]) and its stamp as it
//! stood just before the read ([`Look::stamp`]). `aterm-gui`'s resolver watches every path a read
//! looked at, and owes a session a read where one of them now stands otherwise than it did
//! ([`Look::moved`]): a write after the look moves the stamp, so it is read again, never missed
//! (the footer cause design's §2.3).

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use aterm_uds::file_watch::FileStamp;

/// Which of the footer's inputs a look is of (the footer cause design's §2.3 ids).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Input {
    /// R: the process's registry row, `<claude>/sessions/<pid>.json`.
    Row,
    /// T: the session's transcript, `projects/<slug>/<session>.jsonl`.
    Transcript,
    /// S: its subagents' directories and the transcripts of those still writing.
    Subagents,
    /// G: the git `HEAD` of its working directory, and a worktree's `.git` file.
    Git,
    /// P: its usage tap's reading, `rl/<tab>-<pid>.json`.
    Tap,
    /// L: a login profile (`.claude.json`).
    Profile,
    /// O: the accounts roster.
    Roster,
    /// B: the accounts store's login bindings (`logins.json`).
    Logins,
    /// A: the accounts store's state (`state.json` and the files its load reads).
    Store,
    /// K: the folder a Claude Code build's model catalog is cached in (`upgrade/builds`).
    Catalog,
    /// N: the record of the sign-in this Mac runs now (`signin-running`), which the accounts
    /// outlook says `signing in` by.
    SignIn,
    /// E: a roster row's series of readings on this machine, which the accounts outlook's paces
    /// are read from.
    Series,
}

impl Input {
    /// Every input, in [`Self::index`] order.
    pub const ALL: [Input; 12] = [
        Input::Row,
        Input::Transcript,
        Input::Subagents,
        Input::Git,
        Input::Tap,
        Input::Profile,
        Input::Roster,
        Input::Logins,
        Input::Store,
        Input::Catalog,
        Input::SignIn,
        Input::Series,
    ];

    /// Its place in [`Self::ALL`].
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// The word a dev row or a test names it by.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Input::Row => "row",
            Input::Transcript => "transcript",
            Input::Subagents => "subagents",
            Input::Git => "git",
            Input::Tap => "tap",
            Input::Profile => "profile",
            Input::Roster => "roster",
            Input::Logins => "logins",
            Input::Store => "store",
            Input::Catalog => "catalog",
            Input::SignIn => "sign-in",
            Input::Series => "series",
        }
    }
}

/// What a watch on a looked path is to hear of it where it is a regular file: every change
/// (`All`), or its contents alone (`Contents`: a usage tap's re-report only moves its reading's
/// modification time, which no fact waits on). A directory is heard for its entries, and a path
/// that names nothing at the nearest level of it that is there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Heed {
    All,
    Contents,
}

/// One look of a read at one of its inputs, before it read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Look {
    pub input: Input,
    pub path: PathBuf,
    pub heed: Heed,
    /// What the path named as the read began to read it ([`FileStamp::of`]); `None` where it
    /// named nothing, or where the read took it in before it could be looked at (a subagent
    /// transcript met in the read itself), which any look now differs from.
    pub stamp: Option<FileStamp>,
}

impl Look {
    /// `input` at `path`, as it stands now.
    #[must_use]
    pub fn now(input: Input, path: impl Into<PathBuf>, heed: Heed) -> Look {
        let path = path.into();
        Look {
            input,
            stamp: FileStamp::of(&path),
            path,
            heed,
        }
    }

    /// Whether its path stands otherwise now than it did at the look: one look at it, no open.
    #[must_use]
    pub fn moved(&self) -> bool {
        self.moved_to(FileStamp::of(&self.path))
    }

    /// Whether `now`, the path's stamp, is not the look's.
    #[must_use]
    pub fn moved_to(&self, now: Option<FileStamp>) -> bool {
        now != self.stamp
    }

    /// The path looked at.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

thread_local! {
    /// The looks of the read [`record`] runs on this thread; `None` outside one.
    static LOOKS: RefCell<Option<Vec<Look>>> = const { RefCell::new(None) };
}

/// Run `read` with its looks recorded ([`note`]): what it gave, and each path it looked at,
/// once each (its first look), in the order it first looked. A record inside another keeps its
/// looks to itself.
pub fn record<T>(read: impl FnOnce() -> T) -> (T, Vec<Look>) {
    let outer = LOOKS.with(|looks| looks.borrow_mut().replace(Vec::new()));
    let value = read();
    let looks = LOOKS.with(|looks| std::mem::replace(&mut *looks.borrow_mut(), outer));
    (value, looks.unwrap_or_default())
}

/// Look at `input` at `path` before reading it: its stamp now ([`FileStamp::of`]), noted for the
/// [`record`] this runs inside, if any.
pub fn note(input: Input, path: &Path, heed: Heed) -> Option<FileStamp> {
    let stamp = FileStamp::of(path);
    note_stamped(input, path, heed, stamp);
    stamp
}

/// Note a look at `input` at `path` that found `stamp`, for the [`record`] this runs inside, if
/// any: `None` for a path met in the read itself, before it could be looked at ([`Look::stamp`]).
pub fn note_stamped(input: Input, path: &Path, heed: Heed, stamp: Option<FileStamp>) {
    LOOKS.with(|looks| {
        if let Some(looks) = looks.borrow_mut().as_mut()
            && !looks.iter().any(|look| look.path == path)
        {
            looks.push(Look {
                input,
                path: path.to_path_buf(),
                heed,
                stamp,
            });
        }
    });
}
