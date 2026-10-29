// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! REMOVING WHAT A PASS TAKES, NEVER STOPPED BY ONE ENTRY: a build directory renamed aside, a
//! stage of a build directory's parts, a tooling entry. `std::fs::remove_dir_all` stops at its
//! first error, so one entry it could not remove kept every entry after it, pass after pass: a
//! test scratch's folder carrying `group:everyone deny delete`, left in a build directory's `tmp`
//! by a run killed before its guard cleared the ACL, stranded a 260 GB stage behind it. [`tree`]
//! removes every entry it can, makes removable what aterm's own tools left unremovable, and
//! says what stayed ([`Left`], [`stayed`]). It walks as `remove_dir_all` walks on Unix, by
//! descriptor: each folder is opened from the one holding it, never through a link, and every
//! entry is read, changed and removed by its name in the folder held open, so no path is looked
//! up again, a folder swapped for a link mid-walk leads nowhere outside, and no path is too long.

use std::path::{Path, PathBuf};

/// How many of the entries that stayed a pass's readers name ([`stayed_words`]).
pub const NAMED: usize = 3;

/// What [`tree`] left of what it was asked to remove.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Left {
    /// The entries that stayed: each one that would not go even once made removable, and each
    /// left as it was (another user's, on another volume, one whose attributes cannot be read,
    /// or a file named outside the tree too that refuses). A folder is counted only when
    /// nothing in it was (it is then itself what stayed); one that holds an entry that stayed
    /// stays with it, uncounted.
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
/// already. Off Unix, `std::fs::remove_dir_all`, all of `top` staying when it fails.
#[must_use]
pub fn tree(top: &Path) -> Left {
    sys::tree(top)
}

/// The first `most` entries that stand in `top` now, as a removal that could not finish left them
/// ([`tree`]): each with nothing of it left to walk (a file, a link, a folder that is empty,
/// unreadable, on another volume than the folder holding `top`, or another user's), depth first
/// in name order, never through a link.
#[must_use]
pub fn stayed(top: &Path, most: usize) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if most == 0 || std::fs::symlink_metadata(top).is_err() {
        return found;
    }
    let device = sys::holder_device(top);
    let mut stack = vec![top.to_path_buf()];
    while let Some(path) = stack.pop() {
        let walked =
            std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir() && sys::walked(&m, device));
        let mut children: Vec<PathBuf> = if walked {
            std::fs::read_dir(&path)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .collect()
        } else {
            Vec::new()
        };
        if children.is_empty() {
            found.push(path);
            if found.len() >= most {
                break;
            }
        } else {
            children.sort();
            stack.extend(children.into_iter().rev());
        }
    }
    found
}

/// What stayed of a stage, as its readers name it ([`named`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named {
    /// How many entries stayed ([`Left::stayed`]).
    pub count: usize,
    /// The first [`NAMED`] of them (never more than `count`) as they stand now ([`stayed`]),
    /// each under the stage (the stage itself when it is all that stands).
    pub first: Vec<PathBuf>,
}

impl Named {
    /// How many that stayed are not named.
    #[must_use]
    pub const fn more(&self) -> usize {
        self.count.saturating_sub(self.first.len())
    }

    /// The words a pass's readers print: `<n> entr(y|ies) of <stage> stayed: <first>, … and <m>
    /// more`; never what they hold.
    #[must_use]
    pub fn words(&self, stage: &Path) -> String {
        let mut words = format!(
            "{} entr{} of {} stayed",
            self.count,
            if self.count == 1 { "y" } else { "ies" },
            stage.display()
        );
        if !self.first.is_empty() {
            let named: Vec<String> = self.first.iter().map(|p| p.display().to_string()).collect();
            words.push_str(": ");
            words.push_str(&named.join(", "));
            if self.more() > 0 {
                words.push_str(&format!(" and {} more", self.more()));
            }
        }
        words
    }
}

/// What stayed of `stage` after a removal that could not finish, `count` entries of it
/// ([`Left::stayed`]): the first [`NAMED`] (at most `count`) as they stand now ([`stayed`]),
/// each under `stage`.
#[must_use]
pub fn named(stage: &Path, count: usize) -> Named {
    let first = stayed(stage, NAMED.min(count))
        .into_iter()
        .map(|p| match p.strip_prefix(stage) {
            Ok(rel) if !rel.as_os_str().is_empty() => rel.to_path_buf(),
            _ => p,
        })
        .collect();
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
    use std::fs::Metadata;
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::MetadataExt as _;
    use std::path::Path;

    use aterm_dirfd::{
        self as dirfd, AsFd as _, AtFlags, BorrowedFd, Dir, Errno, Mode, OFlags, OwnedFd,
        RenameFlags, Stat,
    };

    use super::Left;

    /// A folder's owner bits: read, write, search.
    const OWNER: u32 = 0o700;
    /// How many folders deep a walk goes: a folder this deep is moved up to sit directly under
    /// `top` (the same volume, inside the tree) and walked from there, so a walk holds at most
    /// this many folders open (one descriptor each, as `remove_dir_all` holds) and as many
    /// frames, and no tree is too deep to go (a build directory is a few dozen deep).
    pub(super) const DEPTH: usize = 64;
    /// How many times a folder emptied of all it listed is listed again when it will still not
    /// go for what it holds (a listing read as the folder empties may skip an entry): each time
    /// only after a listing removed something.
    const SWEEPS: usize = 8;
    /// The name a folder moved up takes under `top`: `<UP>-<n>`.
    const UP: &str = ".removing-deep";

    /// The folder holding `top` by its path, as [`tree`] opens it (`.` for a bare name).
    fn holder_of(top: &Path) -> &Path {
        top.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
    }

    /// The fields of `meta` a walk judges an entry by.
    fn stat_of(meta: &Metadata) -> Stat {
        Stat {
            st_dev: meta.dev(),
            st_ino: meta.ino(),
            st_mode: meta.mode(),
            st_nlink: meta.nlink(),
            st_uid: meta.uid(),
            st_blocks: meta.blocks(),
        }
    }

    /// The volume of the folder holding `top`, as [`tree`] reads it.
    pub(super) fn holder_device(top: &Path) -> Option<u64> {
        std::fs::metadata(holder_of(top))
            .ok()
            .map(|m| seams::seen(stat_of(&m)).st_dev)
    }

    /// Whether a walk enters the folder read as `meta`: this user's, on `device`.
    pub(super) fn walked(meta: &Metadata, device: Option<u64>) -> bool {
        let stat = seams::seen(stat_of(meta));
        Some(stat.st_dev) == device && stat.st_uid == crate::platform::our_uid()
    }

    pub(super) fn tree(top: &Path) -> Left {
        let mut left = Left::default();
        // A walk again only when the first cleared the ACL of a file it had already left under
        // another of its names (every name of it met in the tree): that name goes now.
        for _ in 0..2 {
            let (walked, again) = walk(top);
            left.freed = left.freed.saturating_add(walked.freed);
            left.stayed = walked.stayed;
            if walked.whole() || !again {
                break;
            }
        }
        left
    }

    /// One walk of `top`: what it left, and whether a walk again would remove more.
    fn walk(top: &Path) -> (Left, bool) {
        let Some(name) = top.file_name() else {
            return (one(), false);
        };
        // `top` is looked up by its path once, as `remove_dir_all` looks it up: the folder
        // holding it opened (through whatever links its path holds, as any path is), and `top`
        // named in that folder from then on.
        let holder = match open_holder(holder_of(top)) {
            Ok(fd) => fd,
            Err(e) if e.is_noent() => return (Left::default(), false),
            Err(_) => return (one(), false),
        };
        let (held, stat) = match (
            dirfd::fstat(&holder),
            dirfd::fstatat(&holder, name, AtFlags::SYMLINK_NOFOLLOW),
        ) {
            (_, Err(e)) if e.is_noent() => return (Left::default(), false),
            (Ok(held), Ok(stat)) => (seams::seen(held), seams::seen(stat)),
            _ => return (one(), false),
        };
        // A volume mounted at `top` reads as on its own volume: it is never entered.
        if stat.st_dev != held.st_dev {
            return (one(), false);
        }
        let mut walk = Walk {
            device: held.st_dev,
            uid: crate::platform::our_uid(),
            left: Left::default(),
            top: None,
            up: Vec::new(),
            moved: 0,
            met: HashMap::new(),
            again: false,
        };
        let mut folder = Folder {
            fd: holder.as_fd(),
            ours: false,
            fixed: false,
            locked: false,
        };
        walk.entry(&mut folder, name, 0);
        (walk.left, walk.again)
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

    /// One [`tree`]: on which volume, as whom, and what it left.
    struct Walk {
        device: u64,
        uid: u32,
        left: Left,
        /// `top` held open once it is, where a folder too deep is moved up ([`Walk::move_up`]).
        top: Option<OwnedFd>,
        /// The folders moved up under `top`, by their names there, to walk from there.
        up: Vec<OsString>,
        /// How many names under `top` moving up has tried.
        moved: u64,
        /// Each file with more than one name (by volume and inode): how many of its names the
        /// walk met and left, each refusing.
        met: HashMap<(u64, u64), u64>,
        /// A file's ACL was cleared after another of its names was left: a walk again finds
        /// that name removable.
        again: bool,
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
        fn make_removable(&mut self) -> bool {
            if !self.ours || self.fixed {
                return false;
            }
            self.fixed = true;
            let cleared = acl::clear_fd(self.fd);
            let owners = match dirfd::fstat(self.fd) {
                Ok(stat) if stat.st_mode & 0o7777 == OWNER => true,
                Ok(_) => dirfd::fchmod(self.fd, mode(OWNER)).is_ok(),
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
        /// It is left as it is: another user's, on another volume, or unreadable.
        Stays,
        /// It is this user's, on this volume, read as this.
        Ours(Stat),
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
    }

    impl Walk {
        /// Remove the entry `name` of `folder`: whether it went. When it did not, something was
        /// counted, here or below it.
        fn entry(&mut self, folder: &mut Folder<'_>, name: &OsStr, depth: usize) -> bool {
            // A name read again once after it changed under the walk; a second change stays.
            for _ in 0..2 {
                let stat = match self.judge(folder, name) {
                    Judged::Gone => return true,
                    Judged::Stays => return self.stays(),
                    Judged::Ours(stat) => stat,
                };
                let mut tried = self.once(folder, name, &stat, depth);
                if matches!(tried, Tried::Refused) {
                    seams::at(seams::At::Refused, name);
                    tried = self.retry(folder, name, &stat, depth);
                }
                match tried {
                    Tried::Gone => return true,
                    Tried::Below => return false,
                    Tried::Refused => {
                        if !is_dir(&stat) && stat.st_nlink > 1 {
                            *self.met.entry((stat.st_dev, stat.st_ino)).or_default() += 1;
                        }
                        return self.stays();
                    }
                    Tried::Changed => {}
                }
            }
            self.stays()
        }

        /// One entry stayed: counted, and not gone.
        fn stays(&mut self) -> bool {
            self.left.stayed += 1;
            false
        }

        /// Read the entry `name` of `folder` by its name, never through a link, and judge it.
        fn judge(&mut self, folder: &mut Folder<'_>, name: &OsStr) -> Judged {
            let read = |fd| dirfd::fstatat(fd, name, AtFlags::SYMLINK_NOFOLLOW).map(seams::seen);
            let stat = match read(folder.fd) {
                Ok(stat) => stat,
                Err(e) if e.is_noent() => return Judged::Gone,
                // A folder denying its search (`deny search`) lets nothing in it be read: made
                // removable, it is read once more.
                Err(e) if denied(e) && folder.make_removable() => match read(folder.fd) {
                    Ok(stat) => stat,
                    Err(e) if e.is_noent() => return Judged::Gone,
                    Err(_) => return Judged::Stays,
                },
                // An entry denying the reading of its own attributes (`deny readattr`) cannot be
                // judged (whose it is, on which volume, under how many names): it stays.
                Err(_) => return Judged::Stays,
            };
            if stat.st_dev != self.device || stat.st_uid != self.uid {
                return Judged::Stays;
            }
            Judged::Ours(stat)
        }

        /// One try at the entry `name` of `folder`, read as `stat` (this user's, on this volume):
        /// a folder is opened (and checked to be the one read), given its owner's bits back
        /// through its descriptor, emptied, and removed; one this deep is moved up.
        fn once(
            &mut self,
            folder: &mut Folder<'_>,
            name: &OsStr,
            stat: &Stat,
            depth: usize,
        ) -> Tried {
            if !is_dir(stat) {
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
            if depth >= DEPTH {
                return self.move_up(folder, name);
            }
            seams::at(seams::At::Opening, name);
            let fd = match dirfd::openat(folder.fd, name, dir_flags(), Mode::empty()) {
                Ok(fd) => fd,
                Err(e) if e.is_noent() => return Tried::Gone,
                Err(e) if matches!(e.raw_os_error(), libc::ENOTDIR | libc::ELOOP) => {
                    return Tried::Changed;
                }
                Err(_) => return Tried::Refused,
            };
            match dirfd::fstat(&fd).map(seams::seen) {
                Ok(held) if same(&held, stat) => {
                    // Its owner's bits back through the folder held open, before it is emptied.
                    if held.st_mode & OWNER != OWNER {
                        let _ = dirfd::fchmod(&fd, mode((held.st_mode | OWNER) & 0o7777));
                    }
                }
                Ok(_) => return Tried::Changed,
                Err(_) => return Tried::Refused,
            }
            if depth == 0 {
                self.top = fd.try_clone().ok();
            }
            let Ok(mut listing) = Dir::new(fd) else {
                return Tried::Refused;
            };
            let mut state = (false, false);
            let (mut sweeps, mut cleared) = (0, false);
            loop {
                let emptied = self.empty(&mut listing, &mut state, depth + 1);
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
                let removed = loop {
                    match dirfd::unlinkat(folder.fd, name, AtFlags::REMOVEDIR) {
                        // Its own ACL may deny its removal (`deny delete`): cleared through the
                        // folder held open, the very one emptied, and removed once more.
                        Err(e) if denied(e) && !cleared => {
                            cleared = true;
                            acl::clear_fd(listing.as_fd());
                        }
                        removed => break removed,
                    }
                };
                match removed {
                    Ok(()) => return Tried::Gone,
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
                    Err(_) => return Tried::Refused,
                }
            }
        }

        /// TRY A REFUSING ENTRY ONCE MORE, made removable as aterm's own tools may have left it:
        /// its folder made removable and its names its owner's alone ([`Folder::make_removable`]),
        /// then the entry read again by its name there. One no longer the entry judged (swapped
        /// since) is read again ([`Tried::Changed`]); for the one judged, in a folder no other
        /// user can rename in, its own extended ACL is cleared by its name, never through a
        /// link, and a folder missing its owner's bits gets exactly them; a file with a name the
        /// walk did not meet (one that may stand outside `top`) is never changed. Tried again
        /// only when something was changed.
        fn retry(
            &mut self,
            folder: &mut Folder<'_>,
            name: &OsStr,
            judged: &Stat,
            depth: usize,
        ) -> Tried {
            let fixed = folder.make_removable();
            let now = match dirfd::fstatat(folder.fd, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => seams::seen(stat),
                Err(e) if e.is_noent() => return Tried::Gone,
                Err(_) => return Tried::Refused,
            };
            if !same(&now, judged) || now.st_uid != self.uid {
                return Tried::Changed;
            }
            let mut changed = false;
            if folder.locked && self.every_name_met(&now) {
                seams::at(seams::At::Changing, name);
                changed = acl::clear_at(folder.fd, name);
                if changed && self.met.contains_key(&(now.st_dev, now.st_ino)) {
                    self.again = true;
                }
                if is_dir(&now) && now.st_mode & OWNER != OWNER {
                    changed |=
                        dirfd::fchmodat(folder.fd, name, mode(OWNER), AtFlags::SYMLINK_NOFOLLOW)
                            .is_ok();
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

        /// MOVE THE FOLDER `name` OF `folder` UP, as deep as a walk goes ([`DEPTH`]): renamed,
        /// never over another entry, to a name of its own directly under `top`, on the same
        /// volume and inside the tree, for the walk of `top` to walk from there
        /// ([`Walk::walk_up`]).
        fn move_up(&mut self, folder: &Folder<'_>, name: &OsStr) -> Tried {
            let Some(top) = self.top.as_ref() else {
                return Tried::Refused;
            };
            for _ in 0..8 {
                self.moved += 1;
                let up = OsString::from(format!("{UP}-{}", self.moved));
                match dirfd::renameat_with(folder.fd, name, top, &up, RenameFlags::NOREPLACE) {
                    Ok(()) => {
                        self.up.push(up);
                        return Tried::Gone;
                    }
                    Err(e) if e.raw_os_error() == libc::EEXIST => {}
                    Err(e) if e.is_noent() => return Tried::Gone,
                    Err(_) => return Tried::Refused,
                }
            }
            Tried::Refused
        }

        /// Walk each folder moved up under `top` (held as `top`, its state `state`) from just
        /// under it: whether any went, or `None` when one stayed (counted where it stayed).
        fn walk_up(&mut self, top: BorrowedFd<'_>, state: &mut (bool, bool)) -> Option<bool> {
            let (mut all, mut went_any) = (true, false);
            while let Some(name) = self.up.pop() {
                let mut folder = Folder {
                    fd: top,
                    ours: true,
                    fixed: state.0,
                    locked: state.1,
                };
                if self.entry(&mut folder, &name, 1) {
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
                // A folder moved up under `top` this walk is walked from there ([`Walk::walk_up`]),
                // never also as `top`'s listing comes upon it.
                if name == "."
                    || name == ".."
                    || (depth == 1 && self.up.iter().any(|up| up == name))
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

    /// How emptying a folder ended.
    enum Emptied {
        /// Every entry it listed went; `went_any` when it listed one.
        All { went_any: bool },
        /// Something in it stayed, counted where it stayed.
        Below,
        /// It could not be listed (through): it is itself what stays.
        Unlisted,
    }

    /// Whether `stat` reads a folder.
    const fn is_dir(stat: &Stat) -> bool {
        dirfd::FileType::from_raw_mode(stat.st_mode).is_dir()
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
    /// volume mounted inside reads both ways; and a call at each point a test acts between
    /// two steps of the walk ([`At`]), with the entry's name. Test builds alone: only this
    /// crate's tests mark the walk.
    #[cfg(test)]
    pub(crate) mod seams {
        use std::cell::RefCell;
        use std::ffi::OsStr;

        use aterm_dirfd::Stat;

        type Hook = Box<dyn FnMut(&OsStr)>;

        /// Where in the walk a test's call runs.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub(crate) enum At {
            /// A folder read as one is about to be opened (a test swaps it for a link there).
            Opening,
            /// An entry refused, before its folder is made removable and it is read again.
            Refused,
            /// An entry is about to be changed by its name (its ACL, a folder's mode).
            Changing,
            /// A folder's listing was read to its end, before the folder is removed.
            Listed,
        }

        const POINTS: usize = 4;

        thread_local! {
            static OTHER_USER: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
            static OTHER_VOLUME: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
            static HOOKS: RefCell<[Option<Hook>; POINTS]> = const { RefCell::new([None, None, None, None]) };
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

        /// Forget every mark and call of this thread.
        pub(crate) fn clear() {
            OTHER_USER.with(|m| m.borrow_mut().clear());
            OTHER_VOLUME.with(|m| m.borrow_mut().clear());
            HOOKS.with(|h| *h.borrow_mut() = [None, None, None, None]);
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

        #[derive(Clone, Copy)]
        pub(super) enum At {
            Opening,
            Refused,
            Changing,
            Listed,
        }

        pub(super) const fn seen(stat: Stat) -> Stat {
            stat
        }

        pub(super) const fn at(_: At, _: &OsStr) {}
    }
}

#[cfg(not(unix))]
mod sys {
    use std::fs::Metadata;
    use std::path::Path;

    use super::Left;

    pub(super) fn tree(top: &Path) -> Left {
        match std::fs::remove_dir_all(top) {
            Ok(()) => Left::default(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Left::default(),
            Err(_) => Left {
                stayed: 1,
                freed: 0,
            },
        }
    }

    pub(super) const fn holder_device(_: &Path) -> Option<u64> {
        None
    }

    pub(super) const fn walked(_: &Metadata, _: Option<u64>) -> bool {
        true
    }
}

#[cfg(all(test, unix))]
pub(crate) use sys::seams;

#[cfg(test)]
#[path = "build_cache_remove_tests.rs"]
mod tests;
