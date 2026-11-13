// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

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
