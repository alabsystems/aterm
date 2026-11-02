// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! REMOVING WHAT A PASS TAKES, NEVER STOPPED BY ONE ENTRY: a build directory renamed aside, a
//! stage of a build directory's parts, a tooling entry. `std::fs::remove_dir_all` stops at its
//! first error, so one entry it could not remove kept every entry after it, pass after pass: a
//! test scratch's folder carrying `group:everyone deny delete`, left in a build directory's `tmp`
//! by a run killed before its guard cleared the ACL, stranded a 260 GB stage behind it. [`tree`]
//! removes every entry it can, makes removable what aterm's own tools left unremovable, and
//! counts what stayed ([`Left`]), each entry for a reason ([`Why`]). It walks as `remove_dir_all`
//! walks on Unix, by descriptor: each folder is opened from the one holding it, never through a
//! link, and every entry is read, changed and removed by its name in the folder held open, so no
//! path is looked up again, a folder swapped for a link mid-walk leads nowhere outside, and no
//! path is too long. A read-only walk of the same kind, judged by the same judge and changing
//! nothing, forecasts what a removal will leave, by that removal's rules ([`forecast`]); and a
//! removal that could not finish keeps its own account of what it left, which a pass's readers
//! name, each entry where the removal left it with the reason it gave ([`named`]), what stands
//! being walked again only where no account is kept ([`stayed`]).

use std::path::{Path, PathBuf};

#[cfg(unix)]
use super::registered::Identity;

/// How many of the entries that stayed a pass's readers name ([`stayed_words`]).
pub const NAMED: usize = 3;

/// What [`tree`] left of what it was asked to remove.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Left {
    /// The entries that stayed: each one that would not go even once made removable, and each
    /// left as it was (another user's, on another volume, one whose attributes cannot be read,
    /// or a file named outside the tree too that refuses). A folder is counted only when
    /// nothing in it was (it is then itself what stayed); one that holds an entry that stayed
    /// stays with it, uncounted. A walk a custody read stopped ([`tree`]) counts its unvisited
    /// remainder as one opaque retained root.
    pub stayed: usize,
    /// The bytes on disk that went: each file's allocated blocks, once its last name went.
    pub freed: u64,
}

impl Left {
    /// Nothing stayed: the tree went whole.
    #[must_use]
    pub const fn whole(&self) -> bool {
        self.stayed == 0
    }
}

/// REMOVE THE TREE AT `top`, NEVER STOPPED BY ONE ENTRY: every entry that can go goes, each
/// folder after what it holds, and what stayed is counted ([`Left`]). An entry of this user's that
/// refuses is made removable as aterm's own tools may have left it, and tried once more: its
/// folder's extended ACL is cleared (on macOS: a `deny delete_child` or `deny search` entry) and
/// its mode made its owner's `rwx` alone, so no other user can rename in it; then the entry is
/// read again by its name there and, only if it is still the entry judged, its own ACL is cleared
/// (a `deny delete` entry refuses every remover) and a folder's mode set to its owner's `rwx`. A
/// folder that opens is changed through its descriptor alone. All of it inside `top`, by
/// descriptor: nothing is followed through a link (a link goes as itself), nothing on another
/// volume (mounted inside or at `top`) is entered or removed, the folder holding `top` is never
/// changed, a file with a name the walk did not meet (which may stand outside `top`) is never
/// changed, only unlinked where it lets its name go, an entry whose attributes cannot be read is
/// never acted on, and an entry another user owns is left as it is, never entered, changed or
/// removed: each of those stays, counted once. A folder deeper than the walk holds open is moved
/// up to sit under `top` and walked from there, so no tree is too deep. A missing `top` is gone
/// already. Each folder's custody is read in it once it is held open. A search denial is
/// lifted through its descriptor only after a complete name-only read proves no custody
/// marker is hidden there. Before it is moved up or anything in it is changed or removed,
/// a keep, a marker that cannot be read or one bound to another
/// folder stops the walk there, nothing more goes, and the tree is never whole
/// ([`Left::stayed`] counts at least one).
/// Off Unix, `std::fs::remove_dir_all`, all of `top` staying when it fails.
#[must_use]
pub fn tree(top: &Path) -> Left {
    sys::tree(top)
}

/// Remove while `permitted` stays true, rechecking before descent and mutation boundaries.
/// Once false, this pass stops even if permission later returns. A revoked result reports
/// exact freed bytes without claiming an entry count for the unvisited remainder.
/// This does not make a predicate check and the following filesystem operation atomic. Off
/// Unix, `permitted` is asked once, before [`tree`]'s `std::fs::remove_dir_all`.
///
/// # Errors
/// Returns [`Revoked`] when `permitted` returns false at a checked boundary.
pub fn tree_while(top: &Path, permitted: &dyn Fn() -> bool) -> Result<Left, Revoked> {
    sys::tree_while(top, permitted)
}

/// Remove only the identified payload in its identified holder. A different inode is never
/// adopted by an initial lookup or retry. Protected root names revoke the pass through its
/// pinned descriptor; unreadable names also revoke it.
#[cfg(unix)]
pub(super) fn tree_identified(
    top: &Path,
    holder: &Identity,
    root: &Identity,
    protected: &[&str],
    permitted: &dyn Fn() -> bool,
) -> Result<Left, Revoked> {
    sys::identified(top, Some(holder), root, protected, permitted, true)
}

/// Remove the identified directory only when empty. Foreign siblings of a retired payload
/// remain untouched; an inode replacement is never adopted.
#[cfg(unix)]
pub(super) fn empty_identified(
    top: &Path,
    root: &Identity,
    permitted: &dyn Fn() -> bool,
) -> Result<Left, Revoked> {
    sys::identified(top, None, root, &[], permitted, false)
}

/// Permission was revoked during a walk; the remaining entries were not enumerated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Revoked {
    /// File allocation counted by [`Left::freed`] before the walk observed revocation.
    pub freed: u64,
}

/// How many entries a read-only walk of a tree ([`forecast`], [`stayed`], [`named`]) judges at
/// most, so a dry plan of a 250 GiB stage stays interactive: past it nothing is forecast, and the
/// forecast says so ([`Forecast::whole`]).
pub const VISITS: usize = 100_000;

/// WHY AN ENTRY STAYS, as the removal's one judge gives it ([`tree`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// Another user's: never entered, changed or removed.
    AnotherUser,
    /// On another volume than the folder holding the tree (one mounted inside it or at it):
    /// never entered or removed.
    AnotherVolume,
    /// Its own ACL denies the reading of its attributes, so it cannot be judged (whose it is,
    /// on which volume, under how many names), and it is never acted on.
    AttributesUnreadable,
    /// A file with a name the walk did not meet, which may stand outside the tree: never changed,
    /// so one that refuses its removal stays.
    NameOutsideTree,
    /// This user's, and it still refused its removal once made removable (a flag such as `uchg`,
    /// which the removal never clears, or a folder the removal could not give its owner's bits).
    StillRefusing,
    /// Not the folder an identified removal was given (`tree_identified`, `empty_identified`):
    /// another inode or no longer a folder at that name, or held by another folder than the one
    /// it was given, which it never adopts and never enters.
    NotIdentified,
    /// The tree itself, which the folder holding it does not let a removal reach: that folder
    /// cannot be opened, or its entries cannot be looked up in it. Never entered.
    Unreachable,
    /// A custody or caller check stopped the removal. The remaining root is opaque, not an
    /// exact count or enumeration of its descendants.
    PermissionRevoked,
    /// A folder a removal takes only once it is empty (`empty_identified`, a stage after its
    /// payload went), holding entries that removal was never given: they are never entered, and
    /// the folder stays with them.
    HoldsOthers,
}

impl Why {
    /// What a reader is told of the entry at `path` that stays so: never what it holds. Where an
    /// entry's ACL denies reading its attributes, on macOS (the platform whose ACLs the removal
    /// reads), the words name the command a person runs to clear it, `chmod -N`; elsewhere the
    /// reason alone.
    #[must_use]
    pub fn words(self, path: &Path) -> String {
        match self {
            Self::AnotherUser => "another user's".to_owned(),
            Self::AnotherVolume => "on another volume".to_owned(),
            Self::AttributesUnreadable if cfg!(target_os = "macos") => format!(
                "its ACL denies reading its attributes; clear it with chmod -N {}",
                crate::platform::sh_quote_str(&path.to_string_lossy())
            ),
            Self::AttributesUnreadable => "its attributes cannot be read".to_owned(),
            Self::NameOutsideTree => "a file named outside the tree too".to_owned(),
            Self::StillRefusing => "it still refuses its removal".to_owned(),
            Self::NotIdentified => "not the folder the removal was given".to_owned(),
            Self::Unreachable => "the folder holding it cannot be opened or searched".to_owned(),
            Self::PermissionRevoked => {
                "removal permission revoked; remaining contents not inspected".to_owned()
            }
            Self::HoldsOthers => "it holds entries the removal was not given".to_owned(),
        }
    }
}

/// WHAT OF A TREE STAYS, each entry with why, as a read-only walk of it judged it ([`forecast`]
/// before a removal, [`named`] after one).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Forecast {
    /// How many of the entries walked stay.
    pub count: usize,
    /// The first of them (at most the `most` asked for), depth first in name order, each by its
    /// name under the tree (empty for the tree itself) with why it stays.
    pub first: Vec<(PathBuf, Why)>,
    /// How many entries the walk judged ([`VISITS`] at most).
    pub walked: usize,
    /// Every entry was walked: none past [`VISITS`], and no folder as deep as a removal moves
    /// up to walk again (`DEPTH`) left unread.
    pub whole: bool,
    /// A name standing in the tree that stops an identified removal of it before anything goes
    /// (a package payload's `.git`, `Cargo.toml` or `Targo.toml`: a workspace there is someone's),
    /// when one does; nothing past it is forecast.
    pub stops: Option<PathBuf>,
}

impl Forecast {
    /// The words a dry plan prints of the tree at `top`: `its removal stops before anything goes:
    /// it holds <name>` when a name in it stops the removal, `<n> entr(y|ies) of it will stay:
    /// <first> (<why>), … and <m> more`, and `not forecast past <walked> entries` when the walk
    /// was cut short; `None` when nothing will stay and the whole tree was walked. Never what an
    /// entry holds.
    #[must_use]
    pub fn words(&self, top: &Path) -> Option<String> {
        let mut words = Vec::new();
        if let Some(name) = &self.stops {
            words.push(format!(
                "its removal stops before anything goes: it holds {}",
                name.display()
            ));
        }
        if self.count > 0 {
            words.push(format!(
                "{} entr{} of it will stay{}",
                self.count,
                if self.count == 1 { "y" } else { "ies" },
                listed(&self.first, top, self.count)
            ));
        }
        if !self.whole {
            words.push(format!("not forecast past {} entries", self.walked));
        }
        (!words.is_empty()).then(|| words.join("; "))
    }
}

/// `: <first> (<why>), … and <m> more` of `count` entries under `top`, the first named (`top`
/// itself by its path); nothing when none is named.
fn listed(first: &[(PathBuf, Why)], top: &Path, count: usize) -> String {
    if first.is_empty() {
        return String::new();
    }
    let named: Vec<String> = first
        .iter()
        .map(|(name, why)| {
            let path = under(top, name.clone());
            let shown = if name.as_os_str().is_empty() {
                &path
            } else {
                name
            };
            format!("{} ({})", shown.display(), why.words(&path))
        })
        .collect();
    let more = count.saturating_sub(first.len());
    let mut words = format!(": {}", named.join(", "));
    if more > 0 {
        words.push_str(&format!(" and {more} more"));
    }
    words
}

/// FORECAST WHAT A REMOVAL OF `top` WILL LEAVE ([`tree`]), read and never acted on: the same walk
/// by descriptor (each folder opened `O_NOFOLLOW` from the one holding it, each entry read by its
/// name there, never through a link, nothing on another volume entered) and the same judge,
/// changing nothing (no ACL cleared, no mode set, nothing moved or removed). An entry it
/// forecasts to stay is another user's, on another volume, or one whose attributes cannot be
/// read; so is `top` itself where the folder holding it does not let it be reached, or where it
/// is a folder of this user's that will not open. Unproved custody makes that root opaque;
/// other unreadable children make the forecast incomplete, never a claim that all will go.
/// The removal never changes the folder holding `top`, nor repairs a directory by name.
/// At most [`VISITS`] entries are judged, and the first `most` that stay are named.
#[must_use]
pub fn forecast(top: &Path, most: usize) -> Forecast {
    sys::look(top, most, false, VISITS)
}

/// [`forecast`] of what [`tree_identified`] will leave of `top`, by its rules: a `top` that is
/// not the folder `root` names, or not held by the folder `holder` names, stays whole and is
/// never entered ([`Why::NotIdentified`]), and a `protected` name standing in `top` stops the
/// removal before anything goes ([`Forecast::stops`]).
#[cfg(unix)]
#[must_use]
pub(super) fn forecast_tree_identified(
    top: &Path,
    holder: &Identity,
    root: &Identity,
    protected: &[&str],
    most: usize,
) -> Forecast {
    sys::look_identified(top, most, Some(holder), root, protected, None)
}

/// [`forecast`] of what [`empty_identified`] will leave of `top`, by its rules, once an earlier
/// removal of the same pass took `taken` from it: a `top` that is not the folder `root` names
/// stays ([`Why::NotIdentified`]), and so does one that holds anything else, never entered
/// ([`Why::HoldsOthers`]).
#[cfg(unix)]
#[must_use]
pub(super) fn forecast_empty_identified(
    top: &Path,
    root: &Identity,
    taken: &[&str],
    most: usize,
) -> Forecast {
    sys::look_identified(top, most, None, root, &[], Some(taken))
}

/// Name what the removal of `from` left under `to`, the folder holding it, as the pass's readers
/// are given `to` ([`named`]): a stage whose payload's identified removal did not finish.
#[cfg(unix)]
pub(super) fn named_as(from: &Path, to: &Path) {
    sys::accounts::moved(from, to);
}

/// The first `most` entries that stand in `top` now, as a plain removal that could not finish
/// left them ([`tree`]): each with nothing of it left to walk (a file, a link, a folder that is
/// empty, that cannot be read into, on another volume than the folder holding `top`, or another
/// user's), depth first in name order, walked by descriptor as the removal walks, so no path is
/// too long to name. [`named`] gives a pass's readers the removal's own account where it has it.
#[must_use]
pub fn stayed(top: &Path, most: usize) -> Vec<PathBuf> {
    if most == 0 {
        return Vec::new();
    }
    sys::look(top, most, true, VISITS)
        .first
        .into_iter()
        .map(|(name, _)| under(top, name))
        .collect()
}

/// The entry `name` under `top`: `top` itself for the empty name.
fn under(top: &Path, name: PathBuf) -> PathBuf {
    if name.as_os_str().is_empty() {
        top.to_path_buf()
    } else {
        top.join(name)
    }
}

/// What stayed of a stage, as its readers name it ([`named`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named {
    /// How many entries stayed ([`Left::stayed`]).
    pub count: usize,
    /// The first [`NAMED`] of them (never more than `count`), depth first in name order, as the
    /// removal counted them or else as they stand now ([`named`]), each under the stage (the
    /// stage itself when it is what stayed), with why it stayed.
    pub first: Vec<(PathBuf, Why)>,
}

impl Named {
    /// How many that stayed are not named.
    #[must_use]
    pub const fn more(&self) -> usize {
        self.count.saturating_sub(self.first.len())
    }

    /// The words a pass's readers print: `<n> entr(y|ies) of <stage> stayed: <first> (<why>), …
    /// and <m> more`, each entry's reason its [`Why::words`] (on macOS, for one whose ACL denies
    /// reading its attributes, the `chmod -N` that clears it); never what they hold.
    #[must_use]
    pub fn words(&self, stage: &Path) -> String {
        format!(
            "{} entr{} of {} stayed{}",
            self.count,
            if self.count == 1 { "y" } else { "ies" },
            stage.display(),
            listed(&self.first, stage, self.count)
        )
    }
}

/// What stayed of `stage` after a removal that could not finish, `count` entries of it
/// ([`Left::stayed`]), the first [`NAMED`] of them (at most `count`), each under `stage` with why
/// it stayed. Where this process's last removal of `stage` counted those `count` and `stage` is
/// still the folder it was then, they are the removal's own: the entries it counted, where it
/// left them, each with the reason it gave, by whatever rules it removed by (an identified
/// removal's too: what it never entered is never named inside). Otherwise they are the entries
/// that stand now, walked by descriptor as a plain removal ([`tree`]) walks ([`stayed`]): the
/// judge's reason, or for an entry of this user's, a name outside the tree
/// ([`Why::NameOutsideTree`]) or a refusal ([`Why::StillRefusing`]).
#[must_use]
pub fn named(stage: &Path, count: usize) -> Named {
    let most = NAMED.min(count);
    let first = if most == 0 {
        Vec::new()
    } else {
        sys::account(stage, count)
            .unwrap_or_else(|| sys::look(stage, most, true, VISITS).first)
            .into_iter()
            .take(most)
            .map(|(name, why)| {
                let name = if name.as_os_str().is_empty() {
                    stage.to_path_buf()
                } else {
                    name
                };
                (name, why)
            })
            .collect()
    };
    Named { count, first }
}

/// [`named`]'s words ([`Named::words`]), as a pass's readers print them.
#[must_use]
pub fn stayed_words(stage: &Path, count: usize) -> String {
    named(stage, count).words(stage)
}

#[cfg(unix)]
mod sys {
    use std::collections::HashMap;
    use std::ffi::{OsStr, OsString};
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::{Path, PathBuf};

    use aterm_dirfd::{
        self as dirfd, AsFd as _, AtFlags, BorrowedFd, Dir, Errno, Mode, OFlags, OwnedFd,
        RenameFlags, Stat,
    };

    use super::super::registered::retention::{self, WalkFence};
    use super::{Forecast, Identity, Left, Revoked, Why};

    /// A folder's owner bits: read, write, search.
    const OWNER: u32 = 0o700;
    /// How many folders deep a walk goes: a folder this deep is held only to read its custody,
    /// then moved up to sit directly under `top` (the same volume, inside the tree) and walked
    /// from there, so a walk holds at most this many folders open and as many frames, and the
    /// one it moves by its custody descriptor alone, while its name still reads as the folder
    /// held and, once moved, while custody read again through every folder held still allows
    /// ([`Walk::move_up`]). Each active folder holds its iterator descriptor and one owned
    /// retention descriptor; fixed root/holder descriptors are additional. No tree is too deep
    /// to go (a build directory is a few dozen deep).
    pub(super) const DEPTH: usize = 64;
    /// How many times a folder emptied of all it listed is listed again when it will still not
    /// go for what it holds (a listing read as the folder empties may skip an entry): each time
    /// only after a listing removed something.
    const SWEEPS: usize = 8;
    /// The name a folder moved up takes under `top`: `<UP>-<n>`; and the name one put back takes
    /// in the folder it left when another entry took its own ([`put_back`]).
    pub(super) const UP: &str = ".removing-deep";

    /// The folder holding `top` by its path, as [`tree`] opens it (`.` for a bare name).
    fn holder_of(top: &Path) -> &Path {
        top.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
    }

    pub(super) fn tree(top: &Path) -> Left {
        let (mut left, revoked) = checked_tree(top, None, None, true);
        if revoked {
            // The plain API cannot return Revoked. Keep an opaque account instead of asking
            // its callers to enumerate the very subtree whose custody stopped the walk.
            left.stayed = 1;
            accounts::keep(top, 1, vec![(PathBuf::new(), Why::PermissionRevoked)]);
        }
        left
    }

    pub(super) fn tree_while(top: &Path, permitted: &dyn Fn() -> bool) -> Result<Left, Revoked> {
        outcome(checked_tree(top, Some(permitted), None, true))
    }

    fn outcome((left, revoked): (Left, bool)) -> Result<Left, Revoked> {
        if revoked {
            Err(Revoked { freed: left.freed })
        } else {
            Ok(left)
        }
    }

    struct Expected<'a> {
        holder: Option<&'a Identity>,
        root: &'a Identity,
        protected: &'a [&'a str],
    }

    pub(super) fn identified(
        top: &Path,
        holder: Option<&Identity>,
        root: &Identity,
        protected: &[&str],
        permitted: &dyn Fn() -> bool,
        recursive: bool,
    ) -> Result<Left, Revoked> {
        outcome(checked_tree(
            top,
            Some(permitted),
            Some(Expected {
                holder,
                root,
                protected,
            }),
            recursive,
        ))
    }

    struct Permit<'a> {
        check: Option<&'a dyn Fn() -> bool>,
        stopped: bool,
        root: Option<OwnedFd>,
        protected: &'a [&'a str],
        directories: Vec<std::rc::Weak<OwnedFd>>,
        fence: Option<WalkFence>,
        /// Exact outstanding names survive ACL-repair walks, including an active piece
        /// popped from the current queue. Only positive held-inode retirement removes one.
        escaped: HashMap<OsString, Identity>,
        moved: u64,
    }

    impl Permit<'_> {
        fn allows(&mut self) -> bool {
            if !self.stopped && self.check.is_some_and(|check| !check()) {
                self.stopped = true;
            }
            if !self.stopped
                && let Some(root) = &self.root
            {
                self.stopped = self.protected.iter().any(|name| {
                    !dirfd::fstatat(root, *name, AtFlags::SYMLINK_NOFOLLOW)
                        .is_err_and(|error| error.is_noent())
                });
            }
            self.directories.retain(|fd| fd.strong_count() != 0);
            if !self.stopped {
                self.stopped = self
                    .directories
                    .iter()
                    .filter_map(std::rc::Weak::upgrade)
                    .any(|fd| !self.allows_at(fd.as_ref()));
            }
            !self.stopped
        }

        fn observe(&mut self, fd: &OwnedFd) -> Option<std::rc::Rc<OwnedFd>> {
            let held = std::rc::Rc::new(fd.try_clone().ok()?);
            self.directories.push(std::rc::Rc::downgrade(&held));
            self.allows().then_some(held)
        }

        fn allows_at(&self, fd: impl dirfd::AsFd) -> bool {
            self.fence
                .as_ref()
                .and_then(|fence| fence.allows_at(fd.as_fd()))
                .unwrap_or_else(|| retention::allows_at(fd))
        }
    }

    fn checked_tree<'a>(
        top: &Path,
        check: Option<&'a dyn Fn() -> bool>,
        expected: Option<Expected<'a>>,
        recursive: bool,
    ) -> (Left, bool) {
        let mut permit = Permit {
            check,
            stopped: false,
            root: None,
            protected: expected.as_ref().map_or(&[][..], |value| value.protected),
            directories: Vec::new(),
            fence: None,
            escaped: HashMap::new(),
            moved: 0,
        };
        let mut left = Left::default();
        let mut first = Vec::new();
        // A walk again only when the first cleared the ACL of a file it had already left under
        // another of its names (every name of it met in the tree): that name goes now.
        for _ in 0..2 {
            let walked = walk(top, &mut permit, expected.as_ref(), recursive);
            left.freed = left.freed.saturating_add(walked.left.freed);
            left.stayed = walked.left.stayed;
            first = walked.first;
            if permit.stopped {
                break;
            }
            if walked.left.whole() || !walked.again {
                break;
            }
        }
        // Result-returning callers get Revoked without an enumerated account. The plain
        // wrapper replaces this with its opaque retained-root account.
        let counted = if permit.stopped { 0 } else { left.stayed };
        accounts::keep(top, counted, first);
        (left, permit.stopped)
    }

    /// What one walk of `top` left ([`walk`]).
    #[derive(Default)]
    struct Walked {
        left: Left,
        /// A walk again would remove more.
        again: bool,
        /// The first [`super::NAMED`] entries it counted, depth first in name order, each by its
        /// name under `top` (empty for `top` itself) with why it stayed.
        first: Vec<(PathBuf, Why)>,
    }

    impl Walked {
        /// `top` itself stayed, for `why`, never entered.
        fn top(why: Why) -> Self {
            seams::stayed(why);
            Self {
                left: one(),
                again: false,
                first: vec![(PathBuf::new(), why)],
            }
        }
    }

    /// One walk of `top`: what it left, with why, and whether a walk again would remove more.
    fn walk(
        top: &Path,
        permit: &mut Permit<'_>,
        expected: Option<&Expected<'_>>,
        recursive: bool,
    ) -> Walked {
        permit.root = None;
        if !permit.allows() {
            return Walked {
                left: one(),
                ..Walked::default()
            };
        }
        let Some(name) = top.file_name() else {
            return Walked::top(Why::Unreachable);
        };
        seams::at(seams::At::Starting, name);
        // `top` is looked up by its path once, as `remove_dir_all` looks it up: the folder
        // holding it opened (through whatever links its path holds, as any path is), and `top`
        // named in that folder from then on.
        let holder = match open_holder(holder_of(top)) {
            Ok(fd) => fd,
            Err(e) if e.is_noent() => return Walked::default(),
            Err(_) => return Walked::top(Why::Unreachable),
        };
        let read = dirfd::fstatat(&holder, name, AtFlags::SYMLINK_NOFOLLOW).map(seams::seen);
        let held = match dirfd::fstat(&holder).map(seams::seen) {
            Ok(held) => held,
            Err(_) if expected.is_none() && read.as_ref().is_err_and(|e| e.is_noent()) => {
                return Walked::default();
            }
            Err(_) => return Walked::top(Why::Unreachable),
        };
        let uid = crate::platform::our_uid();
        match top_verdict(&held, read, holder.as_fd(), uid, expected) {
            Judged::Gone => return Walked::default(),
            Judged::Stays(why) => return Walked::top(why),
            Judged::Ours(_) => {}
        }
        let mut walk = Walk {
            device: held.st_dev,
            uid,
            left: Left::default(),
            top: None,
            top_path: top.to_path_buf(),
            up: permit.escaped.keys().cloned().collect(),
            moved: permit.moved,
            active_up: None,
            met: HashMap::new(),
            again: false,
            permit,
            expected_root: expected.map(|value| (value.root.device, value.root.inode)),
            recursive,
            at: PathBuf::new(),
            first: Vec::new(),
        };
        let mut folder = Folder {
            fd: holder.as_fd(),
            ours: false,
            fixed: false,
            locked: false,
        };
        walk.entry(&mut folder, name, 0);
        walk.permit.moved = walk.moved;
        Walked {
            left: walk.left,
            again: walk.again,
            first: walk.first,
        }
    }

    /// THE JUDGE OF `top` AS A WALK FIRST LOOKS AT IT, read as `read` in the folder held open as
    /// `holder` (read as `held`), for the user `uid`, by an identified removal's rules where
    /// `expected` names them: the removal's ([`walk`]) and the read-only walk's ([`look`]). A
    /// `top` that is gone is gone, unless an identified removal finds it held by another folder
    /// than the one it was given; one whose name the folder holding it does not let be looked up
    /// (it denies its search) cannot be reached; the rest is judged by the one judge
    /// ([`verdict`]: on the holder's volume, as `uid`), and then, for an identified removal,
    /// must be the very folder it was given, in the very folder it was given.
    fn top_verdict(
        held: &Stat,
        read: Result<Stat, Errno>,
        holder: BorrowedFd<'_>,
        uid: u32,
        expected: Option<&Expected<'_>>,
    ) -> Judged {
        let held_given = expected
            .and_then(|value| value.holder)
            .is_none_or(|wanted| matches_holder(wanted, held));
        match read {
            Err(e) if e.is_noent() && held_given => Judged::Gone,
            Err(e) if e.is_noent() => Judged::Stays(Why::NotIdentified),
            Err(_) if !searched(holder) => Judged::Stays(Why::Unreachable),
            read => match verdict(read, held.st_dev, uid) {
                Judged::Ours(stat)
                    if !held_given
                        || expected.is_some_and(|value| {
                            !matches_identity(value.root, &stat) || !is_dir(&stat)
                        }) =>
                {
                    Judged::Stays(Why::NotIdentified)
                }
                judged => judged,
            },
        }
    }

    /// The folder holding `top`, opened to read it or, where it denies its reading, to search it
    /// alone (`O_SEARCH`; `O_PATH` on Linux): all a removal of `top` asks of it.
    fn open_holder(holder: &Path) -> Result<OwnedFd, Errno> {
        let flags = OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NONBLOCK;
        match dirfd::openat(dirfd::CWD, holder, OFlags::RDONLY | flags, Mode::empty()) {
            Err(e) if denied(e) => match SEARCH {
                Some(search) => dirfd::openat(dirfd::CWD, holder, search | flags, Mode::empty()),
                None => Err(e),
            },
            opened => opened,
        }
    }

    /// Opening a folder to search it alone, where the platform has it.
    #[cfg(target_vendor = "apple")]
    const SEARCH: Option<OFlags> = Some(OFlags::from_bits(libc::O_SEARCH));
    #[cfg(any(target_os = "linux", target_os = "android"))]
    const SEARCH: Option<OFlags> = Some(OFlags::from_bits(libc::O_PATH));
    #[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
    const SEARCH: Option<OFlags> = None;

    /// All of `top` stayed: it could not be looked at.
    const fn one() -> Left {
        Left {
            stayed: 1,
            freed: 0,
        }
    }

    /// Every folder is opened so: read-only, a folder, never a link, never a device's wait.
    fn dir_flags() -> OFlags {
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK
    }

    /// A refusal that making the entry removable may lift.
    const fn denied(e: Errno) -> bool {
        matches!(e.raw_os_error(), libc::EACCES | libc::EPERM)
    }

    /// Whether `a` and `b` read the same entry: one inode, of one type.
    fn same(a: &Stat, b: &Stat) -> bool {
        a.st_dev == b.st_dev
            && a.st_ino == b.st_ino
            && dirfd::FileType::from_raw_mode(a.st_mode)
                == dirfd::FileType::from_raw_mode(b.st_mode)
    }

    /// A kernel witness armed on the exact held inode before its directory unlink. APFS
    /// keeps a removed directory's link count, lookup and path metadata stale; a vnode's
    /// actual deletion note distinguishes it from a renamed inode or a removed replacement.
    struct Retirement {
        #[cfg(target_os = "macos")]
        watcher: aterm_uds::nofollow::Watcher,
    }

    impl Retirement {
        fn arm(fd: BorrowedFd<'_>) -> Option<Self> {
            #[cfg(target_os = "macos")]
            {
                let watcher = aterm_uds::nofollow::Watcher::new().ok()?;
                watcher.add(fd, 1).ok()?;
                Some(Self { watcher })
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = fd;
                Some(Self {})
            }
        }

        fn observed(&self, fd: BorrowedFd<'_>, held: &Stat) -> bool {
            let Ok(now) = dirfd::fstat(fd) else {
                return false;
            };
            if !same(&now, held) {
                return false;
            }
            #[cfg(target_os = "macos")]
            {
                self.watcher.deleted(1).unwrap_or(false)
            }
            #[cfg(not(target_os = "macos"))]
            {
                now.st_nlink == 0
            }
        }
    }

    /// PUT BACK the folder renamed up to `up` under `top` that its walk may not take from there
    /// ([`Walk::move_up`]), into `folder`, the folder held that it was renamed out of: at `name`,
    /// never over another entry; where another entry took `name` meanwhile, beside it under a
    /// name of its own ([`UP`] and the next of `moved`), never over another entry either. So it
    /// stands again under every folder held above it, one of which may hold the keep that
    /// stopped the pass. Only where neither rename can act (`folder` or `top` no longer this
    /// user's to change, eight names beside it taken) does it stay under `top`, whose durable
    /// provisional keep was armed before the move and remains for every later walk.
    fn put_back(
        top: BorrowedFd<'_>,
        up: &OsStr,
        folder: BorrowedFd<'_>,
        name: &OsStr,
        moved: &mut u64,
    ) -> bool {
        let taken = |e: Errno| e.raw_os_error() == libc::EEXIST;
        match dirfd::renameat_with(top, up, folder, name, RenameFlags::NOREPLACE) {
            Err(e) if taken(e) => {}
            Ok(()) => return dirfd::fsync(top).is_ok() && dirfd::fsync(folder).is_ok(),
            Err(_) => return false,
        }
        for _ in 0..8 {
            *moved += 1;
            let beside = OsString::from(format!("{UP}-{moved}"));
            match dirfd::renameat_with(top, up, folder, &beside, RenameFlags::NOREPLACE) {
                Err(e) if taken(e) => {}
                Ok(()) => return dirfd::fsync(top).is_ok() && dirfd::fsync(folder).is_ok(),
                Err(_) => return false,
            }
        }
        false
    }

    fn matches_identity(expected: &Identity, stat: &Stat) -> bool {
        (expected.device, expected.inode) == (stat.st_dev, stat.st_ino)
    }

    fn matches_holder(expected: &Identity, stat: &Stat) -> bool {
        matches_identity(expected, stat)
            && stat.st_uid == crate::platform::our_uid()
            && stat.st_mode & 0o022 == 0
    }

    /// One [`tree`]: on which volume, as whom, and what it left.
    struct Walk<'a, 'b> {
        device: u64,
        uid: u32,
        left: Left,
        /// `top` held open once it is, where a folder too deep is moved up ([`Walk::move_up`]).
        top: Option<OwnedFd>,
        top_path: PathBuf,
        /// The folders moved up under `top`, by their names there, to walk from there.
        up: Vec<OsString>,
        /// How many names under `top` moving up has tried.
        moved: u64,
        /// Only the explicit top queue may admit its recorded outstanding inode.
        active_up: Option<OsString>,
        /// Each file with more than one name (by volume and inode): how many of its names the
        /// walk met and left, each refusing.
        met: HashMap<(u64, u64), u64>,
        /// A file's ACL was cleared after another of its names was left: a walk again finds
        /// that name removable.
        again: bool,
        permit: &'a mut Permit<'b>,
        expected_root: Option<(u64, u64)>,
        recursive: bool,
        /// The name under `top` of the folder being emptied (empty for `top`, and while `top`
        /// itself is judged in the folder holding it).
        at: PathBuf,
        /// The first [`super::NAMED`] entries counted, depth first in name order ([`Walked`]).
        first: Vec<(PathBuf, Why)>,
    }

    /// A folder being emptied, held open.
    struct Folder<'a> {
        fd: BorrowedFd<'a>,
        /// This walk's to change: inside `top` and this user's. The folder holding `top` never is.
        ours: bool,
        /// Already made removable once ([`Folder::make_removable`]).
        fixed: bool,
        /// Made its owner's alone ([`Folder::make_removable`]): no other user can rename in it,
        /// so an entry of it read again by its name stays the entry read until the walk acts.
        locked: bool,
    }

    impl Folder<'_> {
        /// MAKE THIS FOLDER'S ENTRIES REMOVABLE AND ITS NAMES ITS OWNER'S ALONE, once, where it
        /// is this walk's to change, through its descriptor: its extended ACL cleared (a `deny
        /// delete_child` or `deny search` entry, or one letting another user add or remove a
        /// name) and its mode exactly its owner's `rwx`. Whether it was changed, so what refused
        /// is tried once more; [`Folder::locked`] when both held.
        fn make_removable(&mut self, permit: &mut Permit<'_>) -> bool {
            if !self.ours || self.fixed || !permit.allows() {
                return false;
            }
            self.fixed = true;
            let cleared = acl::clear_fd(self.fd);
            let owners = match dirfd::fstat(self.fd) {
                Ok(stat) if stat.st_mode & 0o7777 == OWNER => true,
                Ok(_) if permit.allows() => dirfd::fchmod(self.fd, mode(OWNER)).is_ok(),
                Ok(_) => false,
                Err(_) => false,
            };
            self.locked = cleared && owners;
            true
        }
    }

    /// The permission bits `bits` as a mode word.
    #[cfg_attr(
        not(target_vendor = "apple"),
        allow(
            clippy::unnecessary_fallible_conversions,
            reason = "mode_t is u16 on Apple and u32 elsewhere"
        )
    )]
    fn mode(bits: u32) -> Mode {
        Mode::from_bits(libc::mode_t::try_from(bits).unwrap_or(0o700))
    }

    /// How judging an entry by its name ended.
    enum Judged {
        /// It is gone already.
        Gone,
        /// It is left as it is, and why: another user's, on another volume, or one whose
        /// attributes cannot be read.
        Stays(Why),
        /// It is this user's, on this volume, read as this.
        Ours(Stat),
    }

    /// THE ONE JUDGE of an entry read by its name as `read`, in a walk of the volume `device` as
    /// the user `uid`: the removal's ([`Walk::judge`]) and the read-only walk's ([`look`]).
    fn verdict(read: Result<Stat, Errno>, device: u64, uid: u32) -> Judged {
        match read {
            Err(e) if e.is_noent() => Judged::Gone,
            // An entry denying the reading of its own attributes (`deny readattr`) cannot be
            // judged (whose it is, on which volume, under how many names): it stays.
            Err(_) => Judged::Stays(Why::AttributesUnreadable),
            Ok(stat) if stat.st_dev != device => Judged::Stays(Why::AnotherVolume),
            Ok(stat) if stat.st_uid != uid => Judged::Stays(Why::AnotherUser),
            Ok(stat) => Judged::Ours(stat),
        }
    }

    /// How one try at an entry ended.
    enum Tried {
        /// It went, or was gone already (a folder moved up has gone from its folder).
        Gone,
        /// Something in the folder stayed, counted where it stayed; the folder stays with it.
        Below,
        /// The entry itself refused.
        Refused,
        /// The name is no longer what it was read as (a folder swapped for a link, a file or
        /// another folder): read again.
        Changed,
        /// A folder a removal takes only once empty holds entries it was not given
        /// ([`Why::HoldsOthers`]): never entered, and nothing about it changed would let it go.
        Holds,
    }

    impl Walk<'_, '_> {
        /// Admit a fresh directory before any iterator uses its offset. If search is denied,
        /// only a complete name-only scan can prove that permission repair cannot alter a
        /// retained or unreadable custody declaration. Ancestor and identity checks still
        /// precede repair, and the ordinary custody check follows it.
        fn admit_directory(
            &mut self,
            folder: &Folder<'_>,
            name: &OsStr,
            fd: &OwnedFd,
            held: &Stat,
        ) -> bool {
            if self.permit.allows_at(fd) {
                return true;
            }
            if !dirfd::fstatat(fd, ".", AtFlags::SYMLINK_NOFOLLOW).is_err_and(denied)
                || !unmarked_fresh_directory(fd)
            {
                return false;
            }
            seams::at(seams::At::CustodyChecked, name);
            if !unmarked_fresh_directory(fd)
                || !dirfd::fstatat(folder.fd, name, AtFlags::SYMLINK_NOFOLLOW)
                    .is_ok_and(|now| same(&now, held) && now.st_uid == self.uid)
                || !self.permit.allows()
            {
                return false;
            }
            searchable(fd.as_fd(), held, self.permit) && self.permit.allows_at(fd)
        }

        /// Remove the entry `name` of `folder`: whether it went. When it did not, something was
        /// counted, here or below it.
        fn entry(&mut self, folder: &mut Folder<'_>, name: &OsStr, depth: usize) -> bool {
            if !self.permit.allows() {
                return false;
            }
            // A name read again once after it changed under the walk; a second change stays.
            for _ in 0..2 {
                let stat = match self.judge(folder, name) {
                    Judged::Gone => return true,
                    Judged::Stays(why) => return self.stays(name, depth, why),
                    Judged::Ours(stat) => stat,
                };
                if self.permit.escaped.iter().any(|(expected_name, expected)| {
                    matches_identity(expected, &stat)
                        && (depth != 1
                            || expected_name != name
                            || self.active_up.as_deref() != Some(name))
                }) {
                    self.permit.stopped = true;
                    return false;
                }
                if depth == 1
                    && self
                        .permit
                        .escaped
                        .get(name)
                        .is_some_and(|expected| !matches_identity(expected, &stat))
                {
                    self.permit.stopped = true;
                    return false;
                }
                if depth == 0
                    && self.expected_root.is_some_and(|wanted| {
                        wanted != (stat.st_dev, stat.st_ino) || !is_dir(&stat)
                    })
                {
                    return self.stays(name, depth, Why::NotIdentified);
                }
                let mut tried = self.once(folder, name, &stat, depth);
                if self.permit.stopped {
                    return false;
                }
                if matches!(tried, Tried::Refused) {
                    seams::at(seams::At::Refused, name);
                    tried = self.retry(folder, name, &stat, depth);
                }
                match tried {
                    Tried::Gone => return true,
                    Tried::Below => return false,
                    Tried::Holds => return self.stays(name, depth, Why::HoldsOthers),
                    Tried::Refused => {
                        let why = if self.every_name_met(&stat) {
                            Why::StillRefusing
                        } else {
                            Why::NameOutsideTree
                        };
                        if !is_dir(&stat) && stat.st_nlink > 1 {
                            *self.met.entry((stat.st_dev, stat.st_ino)).or_default() += 1;
                        }
                        return self.stays(name, depth, why);
                    }
                    Tried::Changed => {}
                }
            }
            self.stays(name, depth, Why::StillRefusing)
        }

        /// The entry `name`, `depth` deep, stayed, for `why`: counted, named while it is among
        /// the first [`super::NAMED`] in name order, and not gone.
        fn stays(&mut self, name: &OsStr, depth: usize, why: Why) -> bool {
            seams::stayed(why);
            self.left.stayed += 1;
            let rel = if depth == 0 {
                PathBuf::new()
            } else {
                self.at.join(name)
            };
            first_in_order(&mut self.first, rel, why);
            false
        }

        /// Read the entry `name` of `folder` by its name, never through a link, and judge it
        /// ([`verdict`]).
        fn judge(&mut self, folder: &mut Folder<'_>, name: &OsStr) -> Judged {
            let read = |fd| dirfd::fstatat(fd, name, AtFlags::SYMLINK_NOFOLLOW).map(seams::seen);
            let stat = match read(folder.fd) {
                // A folder denying its search (`deny search`) lets nothing in it be read: made
                // removable, it is read once more.
                Err(e) if denied(e) && folder.make_removable(self.permit) => read(folder.fd),
                first => first,
            };
            verdict(stat, self.device, self.uid)
        }

        /// One try at the entry `name` of `folder`, read as `stat` (this user's, on this volume):
        /// a folder is opened (and checked to be the one read), let its owner search it and its
        /// custody read there ([`searchable`], [`Permit::observe`]), given its owner's bits back
        /// through its descriptor, emptied, and removed; one this deep is moved up once that
        /// custody read passed.
        fn once(
            &mut self,
            folder: &mut Folder<'_>,
            name: &OsStr,
            stat: &Stat,
            depth: usize,
        ) -> Tried {
            if !self.permit.allows() {
                return Tried::Below;
            }
            if !is_dir(stat) {
                // A marker publication and its deletion share the held directory's lock.
                // A pass admitted before another pass armed its fence cannot unlink that
                // newer declaration between its last custody check and unlinkat.
                let _publication = if name == super::super::registered::MARKER {
                    let Some(guard) = retention::publication_lock(folder.fd) else {
                        return Tried::Refused;
                    };
                    if !self.permit.allows() {
                        return Tried::Below;
                    }
                    Some(guard)
                } else {
                    None
                };
                return match dirfd::unlinkat(folder.fd, name, AtFlags::empty()) {
                    Ok(()) => {
                        if stat.st_nlink <= 1 {
                            self.left.freed = self
                                .left
                                .freed
                                .saturating_add(stat.st_blocks.saturating_mul(512));
                        }
                        Tried::Gone
                    }
                    Err(e) if e.is_noent() => Tried::Gone,
                    // A folder now (macOS refuses it as `EPERM`, which [`Walk::retry`] reads again).
                    Err(e) if e.raw_os_error() == libc::EISDIR => Tried::Changed,
                    Err(_) => Tried::Refused,
                };
            }
            seams::at(seams::At::Opening, name);
            if !self.permit.allows() {
                return Tried::Below;
            }
            let fd = match dirfd::openat(folder.fd, name, dir_flags(), Mode::empty()) {
                Ok(fd) => fd,
                Err(e) if e.is_noent() => return Tried::Gone,
                Err(e) if matches!(e.raw_os_error(), libc::ENOTDIR | libc::ELOOP) => {
                    return Tried::Changed;
                }
                Err(_) => return Tried::Refused,
            };
            let held = match dirfd::fstat(&fd).map(seams::seen) {
                Ok(held) if same(&held, stat) => held,
                Ok(_) => return Tried::Changed,
                Err(_) => return Tried::Refused,
            };
            seams::at(seams::At::Held, name);
            // An identified removal's root is pinned before anything of it changes, so the
            // names that make it someone's are read in it first ([`Permit::allows`]): one that
            // denies its search cannot show them absent, and the pass stops with that root as
            // it stood, as [`Walk::retry`] leaves an unpinned root.
            if depth == 0 && self.expected_root.is_some() && !self.permit.protected.is_empty() {
                match fd.try_clone() {
                    Ok(root) => self.permit.root = Some(root),
                    Err(_) => {
                        self.permit.stopped = true;
                        return Tried::Below;
                    }
                }
            }
            if !self.permit.allows() {
                return Tried::Below;
            }
            if !self.admit_directory(folder, name, &fd, &held) {
                self.permit.stopped = true;
                return Tried::Below;
            }
            let Some(retention) = self.permit.observe(&fd) else {
                self.permit.stopped = true;
                return Tried::Below;
            };
            // One this deep is moved up only now, held by its custody descriptor alone, which
            // each check before the rename and the one after it read again, and only while its
            // name still reads as the folder held: another folder at that name is read anew, and
            // one that moved in its place all the same, or this one where a keep came on it or
            // above it as it moved, is put back and stops the pass ([`Walk::move_up`]), so a
            // keep is never walked from under `top`.
            if depth >= DEPTH {
                drop(fd);
                let moved = self.move_up(folder, name, &held);
                drop(retention);
                return moved;
            }
            // Its owner's bits back through the folder held open, before it is emptied.
            if !self.permit.allows() {
                return Tried::Below;
            }
            if held.st_mode & OWNER != OWNER {
                let _ = dirfd::fchmod(&fd, mode((held.st_mode | OWNER) & 0o7777));
            }
            if depth == 0 {
                self.top = fd.try_clone().ok();
            }
            if !self.permit.allows() {
                return Tried::Below;
            }
            let Ok(mut listing) = Dir::new(fd) else {
                return Tried::Refused;
            };
            let mut state = (false, false);
            let (mut sweeps, mut cleared) = (0, false);
            loop {
                if depth == 0
                    && !self.permit.escaped.iter().all(|(name, expected)| {
                        dirfd::fstatat(listing.as_fd(), name, AtFlags::SYMLINK_NOFOLLOW)
                            .is_ok_and(|now| matches_identity(expected, &now))
                    })
                {
                    self.permit.stopped = true;
                    return Tried::Below;
                }
                let emptied = if self.recursive {
                    // What it holds is named under it; `top`'s, under `top`.
                    if depth > 0 {
                        self.at.push(name);
                    }
                    let emptied = self.empty(&mut listing, &mut state, depth + 1);
                    if depth > 0 {
                        self.at.pop();
                    }
                    emptied
                } else {
                    Emptied::All { went_any: false }
                };
                seams::at(seams::At::Listed, name);
                let up = if depth == 0 {
                    self.walk_up(listing.as_fd(), &mut state)
                } else {
                    Some(false)
                };
                let went_any = match (emptied, up) {
                    (Emptied::All { went_any }, Some(up_went)) => went_any || up_went,
                    (Emptied::Unlisted, _) => return Tried::Refused,
                    _ => return Tried::Below,
                };
                if depth == 0 && self.permit.fence.is_some() {
                    // All ordinary entries and all moved-up folders went. Withdraw only
                    // our exact marker now; until then even a process crash keeps the top.
                    let fence = self.permit.fence.as_ref().expect("checked above");
                    let Some(top) = self.top.as_ref() else {
                        return Tried::Refused;
                    };
                    if !self.permit.escaped.is_empty() || !fence.disarm(top) {
                        self.permit.stopped = true;
                        return Tried::Below;
                    }
                    self.permit.fence = None;
                    // An ordinary marker restored by disarm is an ordinary entry again.
                    listing.rewind();
                    continue;
                }
                let retirement = if depth == 1
                    && self
                        .permit
                        .escaped
                        .get(name)
                        .is_some_and(|expected| matches_identity(expected, &held))
                {
                    let Some(witness) = Retirement::arm(listing.as_fd()) else {
                        self.permit.stopped = true;
                        return Tried::Below;
                    };
                    Some(witness)
                } else {
                    None
                };
                let removed = loop {
                    if !self.permit.allows() {
                        return Tried::Below;
                    }
                    match dirfd::unlinkat(folder.fd, name, AtFlags::REMOVEDIR) {
                        // Its own ACL may deny its removal (`deny delete`): cleared through the
                        // folder held open, the very one emptied, and removed once more.
                        Err(e) if denied(e) && !cleared && self.permit.allows() => {
                            cleared = true;
                            acl::clear_fd(listing.as_fd());
                        }
                        removed => break removed,
                    }
                };
                match removed {
                    Ok(()) => {
                        if let Some(witness) = retirement {
                            // The name may have been swapped since it was opened. A successful
                            // unlink alone is not proof that our held escaped inode retired.
                            if witness.observed(listing.as_fd(), &held) {
                                self.permit.escaped.remove(name);
                            } else {
                                self.permit.stopped = true;
                            }
                        }
                        return Tried::Gone;
                    }
                    Err(e) if e.is_noent() => return Tried::Gone,
                    Err(e)
                        if went_any
                            && sweeps < SWEEPS
                            && matches!(e.raw_os_error(), libc::ENOTEMPTY | libc::EEXIST) =>
                    {
                        sweeps += 1;
                        listing.rewind();
                    }
                    Err(e) if e.raw_os_error() == libc::ENOTDIR => return Tried::Changed,
                    // A folder taken only once empty holds what this removal was not given.
                    Err(e)
                        if !self.recursive
                            && matches!(e.raw_os_error(), libc::ENOTEMPTY | libc::EEXIST) =>
                    {
                        return Tried::Holds;
                    }
                    Err(_) => return Tried::Refused,
                }
            }
        }

        /// TRY A REFUSING ENTRY ONCE MORE, made removable as aterm's own tools may have left it:
        /// its folder made removable and its names its owner's alone ([`Folder::make_removable`]),
        /// then the entry read again by its name there. One no longer the entry judged (swapped
        /// since) is read again ([`Tried::Changed`]). A directory needs a pinned search handle
        /// and positive custody clearance before descriptor repair; unsupported descriptor
        /// operations retain it. A file or link's extended ACL may be cleared by name, never
        /// through a link, only when every name is within the walk. Tried again only when
        /// something was changed.
        fn retry(
            &mut self,
            folder: &mut Folder<'_>,
            name: &OsStr,
            judged: &Stat,
            depth: usize,
        ) -> Tried {
            if depth == 0
                && self.expected_root.is_some()
                && !self.permit.protected.is_empty()
                && self.permit.root.is_none()
            {
                return Tried::Refused;
            }
            if !self.permit.allows() {
                return Tried::Below;
            }
            let fixed = folder.make_removable(self.permit);
            if self.permit.stopped {
                return Tried::Below;
            }
            let now = match dirfd::fstatat(folder.fd, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => seams::seen(stat),
                Err(e) if e.is_noent() => return Tried::Gone,
                Err(_) => return Tried::Refused,
            };
            if !same(&now, judged) || now.st_uid != self.uid {
                return Tried::Changed;
            }
            if is_dir(&now) {
                // A directory that would not open has not had its custody inspected. A
                // search-only handle can establish that before a deny-list repair. With
                // neither search nor listing access, preserve it rather than chmod by name.
                let Some(search) = SEARCH else {
                    self.permit.stopped = true;
                    return Tried::Below;
                };
                let flags = search
                    | OFlags::DIRECTORY
                    | OFlags::NOFOLLOW
                    | OFlags::CLOEXEC
                    | OFlags::NONBLOCK;
                let fd = match dirfd::openat(folder.fd, name, flags, Mode::empty()) {
                    Ok(fd) => fd,
                    Err(error) if error.is_noent() => return Tried::Gone,
                    Err(_) => {
                        self.permit.stopped = true;
                        return Tried::Below;
                    }
                };
                if !dirfd::fstat(&fd).is_ok_and(|held| same(&held, &now)) {
                    return Tried::Changed;
                }
                let Some(_retention) = self.permit.observe(&fd) else {
                    self.permit.stopped = true;
                    return Tried::Below;
                };
                if !dirfd::fstatat(folder.fd, name, AtFlags::SYMLINK_NOFOLLOW)
                    .is_ok_and(|held| same(&held, &now))
                {
                    return Tried::Changed;
                }
                let mut target = Folder {
                    fd: fd.as_fd(),
                    ours: true,
                    fixed: false,
                    locked: false,
                };
                if target.make_removable(self.permit) {
                    drop(_retention);
                    drop(fd);
                    return self.once(folder, name, &now, depth);
                }
                return Tried::Refused;
            }
            let mut changed = false;
            if folder.locked && self.every_name_met(&now) {
                seams::at(seams::At::Changing, name);
                if !self.permit.allows() {
                    return Tried::Below;
                }
                changed = acl::clear_at(folder.fd, name);
                if changed && self.met.contains_key(&(now.st_dev, now.st_ino)) {
                    self.again = true;
                }
            }
            if fixed || changed {
                self.once(folder, name, &now, depth)
            } else {
                Tried::Refused
            }
        }

        /// Whether every name of the entry read as `stat` is one the walk met: a folder's (it
        /// has one), a file with one name, or one each name of which the walk met and left.
        fn every_name_met(&self, stat: &Stat) -> bool {
            is_dir(stat)
                || stat.st_nlink <= 1
                || self
                    .met
                    .get(&(stat.st_dev, stat.st_ino))
                    .is_some_and(|met| met.saturating_add(1) >= stat.st_nlink)
        }

        /// MOVE THE FOLDER `name` OF `folder` UP, as deep as a walk goes ([`DEPTH`]), once its
        /// custody read passed in it, held as `held` ([`Walk::once`]): renamed, never over
        /// another entry, to a name of its own directly under `top`, on the same volume and inside
        /// the tree, for the walk of `top` to walk from there ([`Walk::walk_up`]). A rename acts
        /// by name, not by the folder held, so `name` is read again just before it, and just
        /// after it what moved is read and custody read again through every folder held (the
        /// one moved, by its custody descriptor, and each above it, [`Permit::allows`]): another
        /// folder at `name` (one swapped in since the custody read) is not moved but read anew
        /// ([`Tried::Changed`]); one that moved all the same (swapped in between that read and
        /// the rename), whose custody was never read, and the folder held where a keep (or a
        /// marker that cannot be read) was published in it or in a folder above it before the
        /// rename acted, are put back into `folder` and stop the pass, never walked: at `name`,
        /// never over another entry, or, where another entry took `name` meanwhile, beside it
        /// under a name of its own ([`put_back`]), so what moved stands again under every folder
        /// held above it and a later walk reads their custody before anything of it goes.
        /// If no restoration name can be used, the top's prearmed durable keep survives this
        /// pass and every later one; no escaped piece depends on this stack staying alive.
        fn move_up(&mut self, folder: &Folder<'_>, name: &OsStr, held: &Stat) -> Tried {
            let Some(top) = self.top.as_ref() else {
                return Tried::Refused;
            };
            if self.permit.fence.is_none() {
                if !self.permit.allows() {
                    return Tried::Below;
                }
                let Some(fence) = WalkFence::arm(top, &self.top_path, || {
                    let mut top = Folder {
                        fd: top.as_fd(),
                        ours: true,
                        fixed: false,
                        locked: false,
                    };
                    // arm holds the publication lock and proves every custody name absent
                    // before this callback. The normal descriptor repair still asks all
                    // caller/ancestor predicates before changing this unmarked top.
                    top.make_removable(self.permit)
                }) else {
                    // An undurable/unknown fence is never claimed protected. No deep move
                    // has happened, so it is safe to stop even if publication partly ran.
                    self.permit.stopped = true;
                    return Tried::Below;
                };
                self.permit.fence = Some(fence);
            }
            for _ in 0..8 {
                if !self.permit.allows() {
                    return Tried::Below;
                }
                match dirfd::fstatat(folder.fd, name, AtFlags::SYMLINK_NOFOLLOW).map(seams::seen) {
                    Ok(now) if same(&now, held) => {}
                    Ok(_) => {
                        seams::at(seams::At::MoveSkipped, name);
                        self.disarm_before_move();
                        return Tried::Changed;
                    }
                    Err(e) if e.is_noent() => {
                        seams::at(seams::At::MoveSkipped, name);
                        self.disarm_before_move();
                        return Tried::Gone;
                    }
                    Err(_) => return Tried::Refused,
                }
                seams::at(seams::At::Moving, name);
                self.moved += 1;
                let up = OsString::from(format!("{UP}-{}", self.moved));
                match dirfd::renameat_with(folder.fd, name, top, &up, RenameFlags::NOREPLACE) {
                    Ok(()) => {
                        seams::at(seams::At::Moved, name);
                        let moved =
                            dirfd::fstatat(top, &up, AtFlags::SYMLINK_NOFOLLOW).map(seams::seen);
                        // Its custody descriptor is still among the folders held, so a keep
                        // published as the rename acted, in it or above it, is read here.
                        if moved.is_ok_and(|at| same(&at, held)) && self.permit.allows() {
                            self.permit.escaped.insert(
                                up.clone(),
                                Identity {
                                    device: held.st_dev,
                                    inode: held.st_ino,
                                },
                            );
                            self.up.push(up);
                            return Tried::Gone;
                        }
                        if put_back(top.as_fd(), &up, folder.fd, name, &mut self.moved)
                            && self.permit.escaped.is_empty()
                            && let Some(fence) = &self.permit.fence
                            && fence.disarm(top)
                        {
                            self.permit.fence = None;
                        }
                        self.permit.stopped = true;
                        return Tried::Below;
                    }
                    Err(e) if e.raw_os_error() == libc::EEXIST => {}
                    Err(e) if e.is_noent() => return Tried::Gone,
                    Err(_) => return Tried::Refused,
                }
            }
            Tried::Refused
        }

        /// No rename was attempted for this candidate. An exact temporary fence can go
        /// only when no earlier moved or currently walked piece remains escaped.
        fn disarm_before_move(&mut self) {
            if self.permit.escaped.is_empty()
                && let (Some(fence), Some(top)) = (&self.permit.fence, &self.top)
                && fence.disarm(top)
            {
                self.permit.fence = None;
            }
        }

        /// Walk each folder moved up under `top` (held as `top`, its state `state`) from just
        /// under it: whether any went, or `None` when one stayed (counted where it stayed).
        fn walk_up(&mut self, top: BorrowedFd<'_>, state: &mut (bool, bool)) -> Option<bool> {
            let (mut all, mut went_any) = (true, false);
            while let Some(name) = self.up.pop() {
                if !self.permit.allows() {
                    return None;
                }
                if !self.permit.escaped.get(&name).is_some_and(|expected| {
                    dirfd::fstatat(top, &name, AtFlags::SYMLINK_NOFOLLOW)
                        .is_ok_and(|now| matches_identity(expected, &now))
                }) {
                    self.permit.stopped = true;
                    return None;
                }
                let mut folder = Folder {
                    fd: top,
                    ours: true,
                    fixed: state.0,
                    locked: state.1,
                };
                self.active_up = Some(name.clone());
                let went = self.entry(&mut folder, &name, 1);
                self.active_up = None;
                if went {
                    if self.permit.escaped.contains_key(&name) {
                        // Gone/changed by a name lookup is not retirement of the held inode.
                        self.permit.stopped = true;
                        return None;
                    }
                    went_any = true;
                } else {
                    all = false;
                }
                *state = (folder.fixed, folder.locked);
            }
            all.then_some(went_any)
        }

        /// Remove every entry `listing` lists, read as it is emptied (as `remove_dir_all` reads
        /// it: a folder of a hundred thousand entries is never held whole); `state` is the
        /// folder's [`Folder::fixed`] and [`Folder::locked`].
        fn empty(&mut self, listing: &mut Dir, state: &mut (bool, bool), depth: usize) -> Emptied {
            let (mut went_any, mut all) = (false, true);
            loop {
                if !self.permit.allows() {
                    return Emptied::Below;
                }
                let child = match listing.next() {
                    None => break,
                    Some(Ok(child)) => child,
                    Some(Err(_)) => {
                        return if all {
                            Emptied::Unlisted
                        } else {
                            Emptied::Below
                        };
                    }
                };
                let name = OsStr::from_bytes(child.file_name().to_bytes());
                if seams::omit_listing(name) {
                    continue;
                }
                // Every outstanding moved folder, including one from an earlier walk, is
                // processed through the identity-bound queue, never ordinary enumeration.
                if name == "."
                    || name == ".."
                    || (depth == 1 && self.permit.escaped.contains_key(name))
                    || (depth == 1
                        && name == super::super::registered::MARKER
                        && self.permit.fence.is_some())
                {
                    continue;
                }
                let mut folder = Folder {
                    fd: listing.as_fd(),
                    ours: true,
                    fixed: state.0,
                    locked: state.1,
                };
                let went = self.entry(&mut folder, name, depth);
                *state = (folder.fixed, folder.locked);
                if went {
                    went_any = true;
                } else {
                    all = false;
                }
            }
            if all {
                Emptied::All { went_any }
            } else {
                Emptied::Below
            }
        }
    }

    /// Read only names before the directory has a live traversal cursor. A dup shares its
    /// offset, so rewind even on refusal. Case-insensitive matching conservatively protects
    /// custody spellings that a case-insensitive filesystem resolves to the same marker.
    fn unmarked_fresh_directory(fd: &OwnedFd) -> bool {
        let Ok(copy) = fd.try_clone() else {
            return false;
        };
        let Ok(mut listing) = Dir::new(copy) else {
            return false;
        };
        let mut absent = true;
        for entry in listing.by_ref() {
            let Ok(entry) = entry else {
                absent = false;
                break;
            };
            let name = entry.file_name().to_bytes();
            if super::super::registered::CUSTODY_MARKERS
                .iter()
                .any(|marker| name.eq_ignore_ascii_case(marker.as_bytes()))
            {
                absent = false;
                break;
            }
        }
        listing.rewind();
        absent
    }

    /// How emptying a folder ended.
    enum Emptied {
        /// Every entry it listed went; `went_any` when it listed one.
        All { went_any: bool },
        /// Something in it stayed, counted where it stayed.
        Below,
        /// It could not be listed (through): it is itself what stays.
        Unlisted,
    }

    /// Keep `(rel, why)` among the first [`super::NAMED`] of `first`, which stays in name order
    /// (depth first: a path's components compared in turn).
    fn first_in_order(first: &mut Vec<(PathBuf, Why)>, rel: PathBuf, why: Why) {
        let at = first.partition_point(|(name, _)| *name < rel);
        if at < super::NAMED {
            first.insert(at, (rel, why));
            first.truncate(super::NAMED);
        }
    }

    /// Whether `stat` reads a folder.
    const fn is_dir(stat: &Stat) -> bool {
        dirfd::FileType::from_raw_mode(stat.st_mode).is_dir()
    }

    /// A READ-ONLY WALK OF `top` ([`super::forecast`], [`super::stayed`], [`super::named`]): by
    /// descriptor as [`tree`] walks (the folder holding `top` opened as the removal opens it,
    /// each folder opened `O_NOFOLLOW` from the one holding it and checked to be the one read,
    /// each entry read by its name there), `top` judged as the removal first judges it
    /// ([`top_verdict`]) and each entry by the removal's own judge ([`verdict`]), and nothing
    /// changed: no ACL cleared, no mode set, nothing moved or removed. With `left`, what stands is
    /// what a removal left, so every entry with nothing of it left to walk is named, one of this
    /// user's as a file with a name the walk did not meet ([`Why::NameOutsideTree`]) or as
    /// refusing ([`Why::StillRefusing`]); without, it is a forecast, and only what the removal
    /// leaves as it is will stay. At most `visits` entries are judged, and the first `most` that
    /// stay are named, by their names under `top`.
    pub(super) fn look(top: &Path, most: usize, left: bool, visits: usize) -> Forecast {
        look_by(top, most, left, visits, None, None)
    }

    /// [`look`] forecasting an identified removal of `top` ([`identified`]): `holder` and `root`
    /// as given to it, its `protected` names, and, for one that takes `top` only once empty
    /// ([`super::empty_identified`]), the names an earlier removal of the same pass takes
    /// (`alone`).
    pub(super) fn look_identified(
        top: &Path,
        most: usize,
        holder: Option<&Identity>,
        root: &Identity,
        protected: &[&str],
        alone: Option<&[&str]>,
    ) -> Forecast {
        let expected = Expected {
            holder,
            root,
            protected,
        };
        look_by(top, most, false, super::VISITS, Some(&expected), alone)
    }

    fn look_by(
        top: &Path,
        most: usize,
        left: bool,
        visits: usize,
        expected: Option<&Expected<'_>>,
        alone: Option<&[&str]>,
    ) -> Forecast {
        let mut look = Look {
            device: 0,
            uid: crate::platform::our_uid(),
            most,
            left,
            visits,
            expected,
            alone,
            out: Forecast {
                whole: true,
                ..Forecast::default()
            },
            met: HashMap::new(),
            files: Vec::new(),
        };
        look.top(top);
        for (at, key, names) in std::mem::take(&mut look.files) {
            if look.met.get(&key).is_some_and(|met| *met >= names) {
                look.out.first[at].1 = Why::StillRefusing;
            }
        }
        look.out
    }

    /// One [`look`]: on which volume, as whom, by which rules, how far, and what it found.
    struct Look<'a> {
        device: u64,
        uid: u32,
        most: usize,
        left: bool,
        visits: usize,
        /// An identified removal's rules ([`Expected`]), as [`walk`] takes them.
        expected: Option<&'a Expected<'a>>,
        /// A removal that takes `top` only once empty, once these names of it went.
        alone: Option<&'a [&'a str]>,
        out: Forecast,
        /// Each file of this user's with more than one name (by volume and inode): how many of
        /// its names stand in the tree.
        met: HashMap<(u64, u64), u64>,
        /// Each such file named: its place in `out.first`, its volume and inode, and how many
        /// names it has.
        files: Vec<(usize, (u64, u64), u64)>,
    }

    /// How opening and listing a folder of this user's ended ([`Look::listed`]).
    enum Listing {
        /// It is gone, or no longer what it was read as.
        Gone,
        /// It will not open.
        Unopened,
        /// It opened, but what it holds cannot be read: it denies its search, or will not list.
        Unread,
        /// Its readable directory does not have proved disposable custody.
        Retained,
        /// Opened and listed: no more names than are left to judge, and one.
        Read(Dir, Vec<OsString>),
    }

    impl Look<'_> {
        /// Walk from the folder holding `top`, as [`walk`] opens it, `top` judged as [`walk`]
        /// judges it ([`top_verdict`]): a missing `top` leaves nothing, and one that stays is
        /// named itself, never entered.
        fn top(&mut self, top: &Path) {
            let Some(name) = top.file_name() else {
                return self.judged(PathBuf::new(), Why::Unreachable);
            };
            let holder = match open_holder(holder_of(top)) {
                Ok(fd) => fd,
                Err(e) if e.is_noent() => return,
                Err(_) => return self.judged(PathBuf::new(), Why::Unreachable),
            };
            let read = dirfd::fstatat(&holder, name, AtFlags::SYMLINK_NOFOLLOW).map(seams::seen);
            let held = match dirfd::fstat(&holder).map(seams::seen) {
                Ok(held) => held,
                Err(_) if self.expected.is_none() && read.as_ref().is_err_and(|e| e.is_noent()) => {
                    return;
                }
                Err(_) => return self.judged(PathBuf::new(), Why::Unreachable),
            };
            self.device = held.st_dev;
            match top_verdict(&held, read, holder.as_fd(), self.uid, self.expected) {
                Judged::Gone => {}
                Judged::Stays(why) => self.judged(PathBuf::new(), why),
                Judged::Ours(_) => self.entry(holder.as_fd(), name, PathBuf::new(), 0),
            }
        }

        /// One entry judged, `rel` under `top`, that stays for `why`.
        fn judged(&mut self, rel: PathBuf, why: Why) {
            self.out.walked += 1;
            self.stays(rel, why);
        }

        /// Judge the entry `name` of `folder`, `rel` under `top`, `depth` deep, and walk it.
        fn entry(&mut self, folder: BorrowedFd<'_>, name: &OsStr, rel: PathBuf, depth: usize) {
            if self.out.walked >= self.visits {
                self.out.whole = false;
                return;
            }
            self.out.walked += 1;
            let read = dirfd::fstatat(folder, name, AtFlags::SYMLINK_NOFOLLOW).map(seams::seen);
            let stat = match verdict(read, self.device, self.uid) {
                Judged::Gone => return,
                Judged::Stays(why) => return self.stays(rel, why),
                Judged::Ours(stat) => stat,
            };
            // An identified removal never adopts a `top` swapped since it was first judged.
            if depth == 0
                && self
                    .expected
                    .is_some_and(|value| !matches_identity(value.root, &stat) || !is_dir(&stat))
            {
                return self.stays(rel, Why::NotIdentified);
            }
            if !is_dir(&stat) {
                if self.left {
                    self.file(rel, &stat);
                }
                return;
            }
            if depth >= DEPTH {
                // A removal moves a folder this deep up to walk it from under `top`: one that
                // stands refused that, and a forecast does not follow it.
                if self.left {
                    self.stays(rel, Why::StillRefusing);
                } else {
                    self.out.whole = false;
                }
                return;
            }
            if depth == 0
                && let Some(taken) = self.alone
            {
                return self.once_empty(folder, name, &stat, rel, taken);
            }
            let (listing, mut names) = match self.listed(folder, name, &stat) {
                Listing::Gone => return,
                // No listing or search proof can establish that hidden custody is absent.
                Listing::Unopened if depth == 0 => {
                    self.out.whole = false;
                    return self.stays(rel, Why::PermissionRevoked);
                }
                // The actual removal may repair this owned folder, then judge its children.
                // A read-only forecast cannot inspect them and must not claim a complete
                // forecast; one that stands after removal is itself named as refusing.
                Listing::Unopened | Listing::Unread => {
                    if self.left {
                        self.stays(rel, Why::StillRefusing);
                    } else {
                        self.out.whole = false;
                    }
                    return;
                }
                Listing::Retained => {
                    self.out.whole = false;
                    self.stays(rel, Why::PermissionRevoked);
                    return;
                }
                Listing::Read(listing, names) => (listing, names),
            };
            if depth == 0
                && let Some(stop) = self.stopped_by(listing.as_fd())
            {
                self.out.stops = Some(stop);
                return;
            }
            if names.is_empty() {
                if self.left {
                    self.stays(rel, Why::StillRefusing);
                }
                return;
            }
            names.sort();
            for child in names {
                let below = rel.join(&child);
                self.entry(listing.as_fd(), &child, below, depth + 1);
            }
        }

        /// A protected name standing in `top`, held open as `fd`, that stops an identified
        /// removal of it at once (as [`Permit::allows`] reads it: any answer but `ENOENT`).
        fn stopped_by(&self, fd: BorrowedFd<'_>) -> Option<PathBuf> {
            let expected = self.expected?;
            expected
                .protected
                .iter()
                .find(|name| {
                    !dirfd::fstatat(fd, **name, AtFlags::SYMLINK_NOFOLLOW)
                        .is_err_and(|error| error.is_noent())
                })
                .map(PathBuf::from)
        }

        /// `top`, the folder `name` of `folder` read as `stat`, as a removal that takes it only
        /// once empty judges it after `taken` went from it: it stays when it will not open
        /// ([`Why::StillRefusing`]) or holds anything else ([`Why::HoldsOthers`]), never entered.
        fn once_empty(
            &mut self,
            folder: BorrowedFd<'_>,
            name: &OsStr,
            stat: &Stat,
            rel: PathBuf,
            taken: &[&str],
        ) {
            let opened = dirfd::openat(folder, name, dir_flags(), Mode::empty())
                .ok()
                .filter(|fd| {
                    dirfd::fstat(fd)
                        .map(seams::seen)
                        .is_ok_and(|held| same(&held, stat))
                });
            let Some(fd) = opened else {
                return self.stays(rel, Why::StillRefusing);
            };
            if !super::super::registered::retention::allows_at(&fd) {
                self.out.whole = false;
                return self.stays(rel, Why::PermissionRevoked);
            }
            let Ok(listing) = Dir::new(fd) else {
                return self.stays(rel, Why::StillRefusing);
            };
            let mut holds = false;
            for child in listing {
                let Ok(child) = child else {
                    return self.stays(rel, Why::StillRefusing);
                };
                let child = OsStr::from_bytes(child.file_name().to_bytes());
                if child != "." && child != ".." && !taken.iter().any(|t| child == *t) {
                    holds = true;
                    break;
                }
            }
            if holds {
                self.stays(rel, Why::HoldsOthers);
            } else if self.left {
                self.stays(rel, Why::StillRefusing);
            }
        }

        /// The folder `name` of `folder`, read as `stat`, opened to read and searched, and the
        /// names it lists (no more than are left to judge, and one).
        fn listed(&self, folder: BorrowedFd<'_>, name: &OsStr, stat: &Stat) -> Listing {
            let fd = match dirfd::openat(folder, name, dir_flags(), Mode::empty()) {
                Ok(fd) => fd,
                Err(e)
                    if e.is_noent() || matches!(e.raw_os_error(), libc::ENOTDIR | libc::ELOOP) =>
                {
                    return Listing::Gone;
                }
                Err(_) => return Listing::Unopened,
            };
            match dirfd::fstat(&fd).map(seams::seen) {
                Ok(held) if same(&held, stat) => {}
                Ok(_) => return Listing::Gone,
                Err(_) => return Listing::Unread,
            }
            if !searched(fd.as_fd()) {
                return Listing::Unread;
            }
            if !super::super::registered::retention::allows_at(&fd) {
                return Listing::Retained;
            }
            let Ok(mut listing) = Dir::new(fd) else {
                return Listing::Unread;
            };
            let room = self
                .visits
                .saturating_sub(self.out.walked)
                .saturating_add(1);
            let mut names = Vec::new();
            for child in listing.by_ref() {
                let Ok(child) = child else {
                    return Listing::Unread;
                };
                let child = OsStr::from_bytes(child.file_name().to_bytes());
                if child == "." || child == ".." {
                    continue;
                }
                names.push(child.to_os_string());
                if names.len() >= room {
                    break;
                }
            }
            Listing::Read(listing, names)
        }

        /// An entry that stays, `rel` under `top`, for `why`: counted, and named while fewer
        /// than `most` are.
        fn stays(&mut self, rel: PathBuf, why: Why) {
            self.out.count += 1;
            if self.out.first.len() < self.most {
                self.out.first.push((rel, why));
            }
        }

        /// A file of this user's that stands after a removal, `rel` under `top`, read as `stat`:
        /// refusing, or (decided once the walk has met all it will) named outside the tree.
        fn file(&mut self, rel: PathBuf, stat: &Stat) {
            if stat.st_nlink > 1 {
                let key = (stat.st_dev, stat.st_ino);
                *self.met.entry(key).or_default() += 1;
                if self.out.first.len() < self.most {
                    self.files.push((self.out.first.len(), key, stat.st_nlink));
                }
                return self.stays(rel, Why::NameOutsideTree);
            }
            self.stays(rel, Why::StillRefusing);
        }
    }

    /// THE REMOVAL'S OWN ACCOUNT OF WHAT STAYED, kept for its readers in this process
    /// ([`super::named`]): for each tree the last removal of which here left entries, how many it
    /// counted and the first [`super::NAMED`] of them in name order, each where it left it with
    /// the reason it gave, and which folder the tree's name read as then (its volume and inode,
    /// by its path). A removal that took the tree whole keeps none. A plain removal stopped by
    /// a permission check keeps only its opaque root, while Result callers receive Revoked.
    /// At most [`KEPT`] trees are kept, the oldest forgotten first.
    pub(super) mod accounts {
        use std::os::unix::fs::MetadataExt as _;
        use std::path::{Path, PathBuf};
        use std::sync::{Mutex, PoisonError};

        use super::super::Why;

        /// How many trees' accounts are kept.
        const KEPT: usize = 64;

        struct Account {
            top: PathBuf,
            stamp: Option<(u64, u64)>,
            count: usize,
            first: Vec<(PathBuf, Why)>,
        }

        static ACCOUNTS: Mutex<Vec<Account>> = Mutex::new(Vec::new());

        /// Which folder `top`'s name reads as now: its volume and inode, by its path.
        fn stamp(top: &Path) -> Option<(u64, u64)> {
            std::fs::symlink_metadata(top)
                .ok()
                .map(|meta| (meta.dev(), meta.ino()))
        }

        fn put(kept: &mut Vec<Account>, account: Account) {
            kept.retain(|old| old.top != account.top);
            if account.count == 0 {
                return;
            }
            if kept.len() >= KEPT {
                kept.remove(0);
            }
            kept.push(account);
        }

        /// The removal of `top` counted `count` entries, the first of them `first`.
        pub(in super::super) fn keep(top: &Path, count: usize, first: Vec<(PathBuf, Why)>) {
            let stamp = if count == 0 { None } else { stamp(top) };
            let account = Account {
                top: top.to_path_buf(),
                stamp,
                count,
                first,
            };
            put(
                &mut ACCOUNTS.lock().unwrap_or_else(PoisonError::into_inner),
                account,
            );
        }

        /// The first entries the last removal of `top` here counted, when it counted `count`
        /// and `top`'s name still reads as the folder it read as then.
        pub(in super::super) fn find(top: &Path, count: usize) -> Option<Vec<(PathBuf, Why)>> {
            let now = stamp(top);
            ACCOUNTS
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .find(|account| {
                    account.top == top && account.count == count && account.stamp == now
                })
                .map(|account| account.first.clone())
        }

        /// The account of `from` is kept as `to`'s, the folder holding it, each entry named
        /// under `to`; `to` keeps none when `from` has none.
        pub(in super::super) fn moved(from: &Path, to: &Path) {
            let Ok(under) = from.strip_prefix(to) else {
                return;
            };
            let stamp = stamp(to);
            let mut kept = ACCOUNTS.lock().unwrap_or_else(PoisonError::into_inner);
            let taken = kept
                .iter()
                .position(|account| account.top == from)
                .map(|at| kept.remove(at));
            let (count, first) = taken.map_or((0, Vec::new()), |account| {
                let first = account
                    .first
                    .into_iter()
                    .map(|(rel, why)| {
                        let rel = if rel.as_os_str().is_empty() {
                            under.to_path_buf()
                        } else {
                            under.join(rel)
                        };
                        (rel, why)
                    })
                    .collect();
                (account.count, first)
            });
            put(
                &mut kept,
                Account {
                    top: to.to_path_buf(),
                    stamp,
                    count,
                    first,
                },
            );
        }
    }

    /// The removal's own account of `top` ([`accounts::find`]).
    pub(super) fn account(top: &Path, count: usize) -> Option<Vec<(PathBuf, Why)>> {
        accounts::find(top, count)
    }

    /// Whether the folder held open as `fd` lets what it holds be read (`.` looked up in it):
    /// one denying its search (its mode, or a `deny search` ACL) refuses that.
    fn searched(fd: BorrowedFd<'_>) -> bool {
        dirfd::fstatat(fd, ".", AtFlags::SYMLINK_NOFOLLOW).is_ok()
    }

    /// A folder's owner's search bit.
    const OWNER_SEARCH: u32 = 0o100;

    /// LET ITS OWNER SEARCH THE FOLDER HELD OPEN AS `fd` (read as `held`), as reading its
    /// custody in it needs, through its descriptor alone: a mode lacking its owner's search
    /// gains that bit alone (never its write), and a folder that still denies its search (a
    /// `deny search` ACL entry) has its whole extended ACL cleared, as
    /// [`Folder::make_removable`] clears one. A folder its owner may search already is left as
    /// it is. One still unsearched reads as keeping its custody ([`Permit::observe`]).
    fn searchable(fd: BorrowedFd<'_>, held: &Stat, permit: &mut Permit<'_>) -> bool {
        if searched(fd) {
            return true;
        }
        if held.st_mode & OWNER_SEARCH == 0
            && permit.allows()
            && dirfd::fchmod(fd, mode((held.st_mode | OWNER_SEARCH) & 0o7777)).is_ok()
            && searched(fd)
        {
            return true;
        }
        permit.allows() && acl::clear_fd(fd) && searched(fd)
    }

    /// CLEARING AN EXTENDED ACL by descriptor, never through a link: `setattrlistat` with
    /// `FSOPT_NOFOLLOW` on a name in a folder held open, `fsetattrlist` on a folder held open.
    /// Either asks for no access to the entry's contents, so an entry denying its own reading
    /// is cleared as well; its owner may always change it. What `chmod -N` does, by name.
    #[cfg(target_os = "macos")]
    mod acl {
        use std::ffi::OsStr;
        use std::os::fd::AsRawFd as _;

        use aterm_dirfd::{Arg as _, BorrowedFd};

        /// `<sys/attr.h>` `struct attrlist`.
        #[repr(C)]
        struct AttrList {
            bitmapcount: u16,
            reserved: u16,
            commonattr: u32,
            volattr: u32,
            dirattr: u32,
            fileattr: u32,
            forkattr: u32,
        }

        /// An `attrreference_t` and the `struct kauth_filesec` it points at, which carries no
        /// entry: `acl_entrycount` [`NO_ACL`] removes the ACL.
        #[repr(C)]
        struct NoAcl {
            dataoffset: i32,
            length: u32,
            magic: u32,
            owner: [u8; 16],
            group: [u8; 16],
            entrycount: u32,
            flags: u32,
        }

        const ATTR_BIT_MAP_COUNT: u16 = 5;
        const ATTR_CMN_EXTENDED_SECURITY: u32 = 0x0040_0000;
        const FSOPT_NOFOLLOW: u32 = 0x0000_0001;
        /// `KAUTH_FILESEC_MAGIC`.
        const FILESEC_MAGIC: u32 = 0x012c_c16d;
        /// `KAUTH_FILESEC_NOACL`.
        const NO_ACL: u32 = u32::MAX;

        // SAFETY (declaration): libSystem exports both with these C signatures on every
        // supported macOS (`setattrlistat` since 10.13); each returns 0, or -1 and errno.
        unsafe extern "C" {
            fn setattrlistat(
                dirfd: libc::c_int,
                path: *const libc::c_char,
                attr_list: *mut libc::c_void,
                attr_buf: *mut libc::c_void,
                attr_buf_size: usize,
                options: u32,
            ) -> libc::c_int;
            fn fsetattrlist(
                fd: libc::c_int,
                attr_list: *mut libc::c_void,
                attr_buf: *mut libc::c_void,
                attr_buf_size: usize,
                options: libc::c_uint,
            ) -> libc::c_int;
        }

        fn request() -> (AttrList, NoAcl) {
            (
                AttrList {
                    bitmapcount: ATTR_BIT_MAP_COUNT,
                    reserved: 0,
                    commonattr: ATTR_CMN_EXTENDED_SECURITY,
                    volattr: 0,
                    dirattr: 0,
                    fileattr: 0,
                    forkattr: 0,
                },
                NoAcl {
                    dataoffset: 8,
                    length: 44,
                    magic: FILESEC_MAGIC,
                    owner: [0; 16],
                    group: [0; 16],
                    entrycount: NO_ACL,
                    flags: 0,
                },
            )
        }

        /// Clear the extended ACL of the entry `name` of the folder `folder`, a link as itself:
        /// whether it was cleared (or held none).
        pub(super) fn clear_at(folder: BorrowedFd<'_>, name: &OsStr) -> bool {
            let (mut list, mut buf) = request();
            name.with_c_str(|c| {
                // SAFETY: `folder` is live, `c` is NUL-terminated, and `list` and `buf` are
                // writable C layouts of the sizes passed, which the call reads and nothing else.
                let rc = unsafe {
                    setattrlistat(
                        folder.as_raw_fd(),
                        c.as_ptr(),
                        (&raw mut list).cast(),
                        (&raw mut buf).cast(),
                        std::mem::size_of::<NoAcl>(),
                        FSOPT_NOFOLLOW,
                    )
                };
                Ok(rc == 0)
            })
            .unwrap_or(false)
        }

        /// Clear the extended ACL of the folder held open as `fd`: whether it was cleared (or
        /// held none).
        pub(super) fn clear_fd(fd: BorrowedFd<'_>) -> bool {
            let (mut list, mut buf) = request();
            // SAFETY: `fd` is live, and `list` and `buf` are writable C layouts of the sizes
            // passed, which the call reads and nothing else.
            let rc = unsafe {
                fsetattrlist(
                    fd.as_raw_fd(),
                    (&raw mut list).cast(),
                    (&raw mut buf).cast(),
                    std::mem::size_of::<NoAcl>(),
                    0,
                )
            };
            rc == 0
        }

        const _: () = assert!(std::mem::size_of::<AttrList>() == 24);
        const _: () = assert!(std::mem::size_of::<NoAcl>() == 52);
    }

    /// Off macOS no extended ACL refuses a removal its owner's bits allow, and a mode of the
    /// owner's `rwx` alone masks every POSIX ACL entry another user holds.
    #[cfg(not(target_os = "macos"))]
    mod acl {
        use std::ffi::OsStr;

        use aterm_dirfd::BorrowedFd;

        pub(super) const fn clear_at(_: BorrowedFd<'_>, _: &OsStr) -> bool {
            false
        }

        pub(super) const fn clear_fd(_: BorrowedFd<'_>) -> bool {
            true
        }
    }

    /// THE TESTS' VIEW OF THE WALK, on the thread that marks it: an entry read as another
    /// user's or as on another volume (by inode), by its name and once opened alike, as a
    /// volume mounted inside reads both ways; a call at each point a test acts between two
    /// steps of the walk ([`At`]), with the entry's name; and why each entry the removal
    /// counted stayed ([`judged`]). Test builds alone: only this crate's tests mark the walk.
    #[cfg(test)]
    pub(crate) mod seams {
        use std::cell::RefCell;
        use std::ffi::{OsStr, OsString};

        use aterm_dirfd::Stat;

        use super::super::Why;

        type Hook = Box<dyn FnMut(&OsStr)>;

        /// Where in the walk a test's call runs.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub(crate) enum At {
            /// The caller's authorization has reached the walk, before its first holder lookup.
            Starting,
            /// A folder read as one is about to be opened (a test swaps it for a link there).
            Opening,
            /// A folder was opened and checked to be the one read, before its custody is read in
            /// it (a test publishes custody above it there).
            Held,
            /// A fresh directory's complete listing proved no custody marker, before repair.
            CustodyChecked,
            /// An entry refused, before its folder is made removable and it is read again.
            Refused,
            /// An entry is about to be changed by its name (its ACL, a folder's mode).
            Changing,
            /// A folder's listing was read to its end, before the folder is removed.
            Listed,
            /// A folder as deep as the walk goes was read again by its name as the one held, just
            /// before it is renamed up (a test swaps another folder in there).
            Moving,
            /// A folder as deep as the walk goes was renamed up, before what moved and custody
            /// are read again (a test takes the name it left there).
            Moved,
            /// A deep candidate changed or vanished before any rename, before its temporary
            /// fence can be withdrawn (a test revokes the caller at this boundary).
            MoveSkipped,
        }

        const POINTS: usize = 10;

        thread_local! {
            static OTHER_USER: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
            static OTHER_VOLUME: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
            static HOOKS: RefCell<[Option<Hook>; POINTS]> = const { RefCell::new([const { None }; POINTS]) };
            static STAYED: RefCell<Vec<Why>> = const { RefCell::new(Vec::new()) };
            static OMIT: RefCell<Vec<OsString>> = const { RefCell::new(Vec::new()) };
            static OMITTED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
        }

        /// Read the entry with inode `ino` as another user's, on this thread.
        pub(crate) fn other_user(ino: u64) {
            OTHER_USER.with(|m| m.borrow_mut().push(ino));
        }

        /// Read the entry with inode `ino` as on another volume, on this thread.
        pub(crate) fn other_volume(ino: u64) {
            OTHER_VOLUME.with(|m| m.borrow_mut().push(ino));
        }

        /// Call `then` with the entry's name each time the walk is at `at`, on this thread.
        pub(crate) fn on(at: At, then: impl FnMut(&OsStr) + 'static) {
            HOOKS.with(|h| h.borrow_mut()[at as usize] = Some(Box::new(then)));
        }

        /// Omit this name from actual directory enumeration, as a mutating cursor may do.
        #[cfg(target_os = "macos")]
        pub(crate) fn omit(name: &OsStr) {
            OMIT.with(|names| names.borrow_mut().push(name.to_owned()));
        }

        #[cfg(target_os = "macos")]
        pub(crate) fn omitted() -> usize {
            OMITTED.with(std::cell::Cell::get)
        }

        pub(super) fn omit_listing(name: &OsStr) -> bool {
            let omit = OMIT.with(|names| names.borrow().iter().any(|item| item == name));
            if omit {
                OMITTED.with(|count| count.set(count.get() + 1));
            }
            omit
        }

        /// Why each entry the removal counted as staying on this thread stayed, in the order
        /// counted, since the last [`clear`].
        pub(crate) fn judged() -> Vec<Why> {
            STAYED.with(|s| s.borrow().clone())
        }

        /// Forget every mark and call of this thread.
        pub(crate) fn clear() {
            OTHER_USER.with(|m| m.borrow_mut().clear());
            OTHER_VOLUME.with(|m| m.borrow_mut().clear());
            HOOKS.with(|h| *h.borrow_mut() = [const { None }; POINTS]);
            STAYED.with(|s| s.borrow_mut().clear());
            OMIT.with(|names| names.borrow_mut().clear());
            OMITTED.with(|count| count.set(0));
        }

        pub(super) fn stayed(why: Why) {
            STAYED.with(|s| s.borrow_mut().push(why));
        }

        pub(super) fn seen(mut stat: Stat) -> Stat {
            if OTHER_USER.with(|m| m.borrow().contains(&stat.st_ino)) {
                stat.st_uid = stat.st_uid.wrapping_add(1);
            }
            if OTHER_VOLUME.with(|m| m.borrow().contains(&stat.st_ino)) {
                stat.st_dev = stat.st_dev.wrapping_add(1);
            }
            stat
        }

        pub(super) fn at(at: At, name: &OsStr) {
            let call = HOOKS.with(|h| h.borrow_mut()[at as usize].take());
            if let Some(mut call) = call {
                call(name);
                HOOKS.with(|h| {
                    let slot = &mut h.borrow_mut()[at as usize];
                    if slot.is_none() {
                        *slot = Some(call);
                    }
                });
            }
        }
    }

    #[cfg(not(test))]
    mod seams {
        use std::ffi::OsStr;

        use aterm_dirfd::Stat;

        use super::super::Why;

        #[derive(Clone, Copy)]
        pub(super) enum At {
            Starting,
            Opening,
            Held,
            CustodyChecked,
            Refused,
            Changing,
            Listed,
            Moving,
            Moved,
            MoveSkipped,
        }

        pub(super) const fn seen(stat: Stat) -> Stat {
            stat
        }

        pub(super) const fn omit_listing(_: &OsStr) -> bool {
            false
        }

        pub(super) const fn at(_: At, _: &OsStr) {}

        pub(super) const fn stayed(_: Why) {}
    }
}

#[cfg(not(unix))]
mod sys {
    use std::path::{Path, PathBuf};

    use super::{Forecast, Left, Revoked, Why};

    /// [`tree`] once `permitted` says so: `remove_dir_all` has no boundary to ask again at.
    pub(super) fn tree_while(top: &Path, permitted: &dyn Fn() -> bool) -> Result<Left, Revoked> {
        if permitted() {
            Ok(tree(top))
        } else {
            Err(Revoked { freed: 0 })
        }
    }

    fn clear_readonly(path: &Path, meta: &std::fs::Metadata) {
        let mut perms = meta.permissions();
        if perms.readonly() {
            #[expect(
                clippy::permissions_set_readonly_false,
                reason = "this #[cfg(not(unix))] module builds off Unix, clearing Windows FILE_ATTRIBUTE_READONLY on git packfiles"
            )]
            perms.set_readonly(false);
            let _ = std::fs::set_permissions(path, perms);
        }
    }

    fn clear_readonly_tree(top: &Path) {
        let mut stack = vec![top.to_path_buf()];
        while let Some(path) = stack.pop() {
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            clear_readonly(&path, &meta);
            if meta.is_dir()
                && let Ok(entries) = std::fs::read_dir(&path)
            {
                stack.extend(entries.flatten().map(|e| e.path()));
            }
        }
    }

    pub(super) fn tree(top: &Path) -> Left {
        let meta = match std::fs::symlink_metadata(top) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Left::default(),
            Err(_) => {
                return Left {
                    stayed: 1,
                    freed: 0,
                };
            }
        };
        let removed = if meta.file_type().is_symlink() {
            std::fs::remove_file(top).or_else(|_| std::fs::remove_dir(top))
        } else if meta.is_dir() {
            clear_readonly_tree(top);
            std::fs::remove_dir_all(top)
        } else {
            clear_readonly(top, &meta);
            std::fs::remove_file(top)
        };
        match removed {
            Ok(()) => Left::default(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Left::default(),
            Err(_) => Left {
                stayed: 1,
                freed: 0,
            },
        }
    }

    /// Off Unix a removal keeps no account of what stayed: its readers walk what stands.
    pub(super) const fn account(_: &Path, _: usize) -> Option<Vec<(PathBuf, Why)>> {
        None
    }

    /// Off Unix a removal is `remove_dir_all`, which no judge leads: nothing is forecast
    /// (none walked, [`Forecast::whole`] false); after one, each entry with nothing of it left
    /// to walk stands as refusing, walked by path in name order.
    pub(super) fn look(top: &Path, most: usize, left: bool, visits: usize) -> Forecast {
        let mut out = Forecast::default();
        if !left || std::fs::symlink_metadata(top).is_err() {
            out.whole = left;
            return out;
        }
        out.whole = true;
        let mut stack = vec![PathBuf::new()];
        while let Some(rel) = stack.pop() {
            if out.walked >= visits {
                out.whole = false;
                break;
            }
            out.walked += 1;
            let path = if rel.as_os_str().is_empty() {
                top.to_path_buf()
            } else {
                top.join(&rel)
            };
            let mut children: Vec<PathBuf> =
                if std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir()) {
                    std::fs::read_dir(&path)
                        .into_iter()
                        .flatten()
                        .flatten()
                        .map(|e| rel.join(e.file_name()))
                        .collect()
                } else {
                    Vec::new()
                };
            if children.is_empty() {
                out.count += 1;
                if out.first.len() < most {
                    out.first.push((rel, Why::StillRefusing));
                }
            } else {
                children.sort();
                stack.extend(children.into_iter().rev());
            }
        }
        out
    }
}

#[cfg(all(test, unix))]
pub(crate) use sys::seams;

#[cfg(test)]
#[path = "build_cache_remove_tests.rs"]
mod tests;
