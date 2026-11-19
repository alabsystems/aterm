// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::{Cell, RefCell};

thread_local! {
    static CUSTODY: Cell<u64> = const { Cell::new(0) };
    static TOWARD: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
    static BETWEEN: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
    static STANDING: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
    static CARRIED: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
    static UNKEYED: Cell<Option<super::materialize::Unkeyed>> = const { Cell::new(None) };
    static BUILD: RefCell<Option<String>> = const { RefCell::new(None) };
    static BOUND: Cell<Option<[u8; 16]>> = const { Cell::new(None) };
    static LEFT: Cell<Option<[u8; 16]>> = const { Cell::new(None) };
    static UNREAD: Cell<bool> = const { Cell::new(false) };
}

/// Leave `part` out of the projection key this thread's passes make (or, for
/// `Unkeyed::History` and `Unkeyed::Snapshot`, out of the skip): the negative control of the law
/// that each part is needed.
pub(crate) fn leave_out(part: super::materialize::Unkeyed) {
    UNKEYED.with(|slot| slot.set(Some(part)));
}

/// Which part this thread's key leaves out.
pub(crate) fn unkeyed() -> impl Fn(super::materialize::Unkeyed) -> bool {
    let left = UNKEYED.with(Cell::get);
    move |part| left == Some(part)
}

/// Make this thread's passes a later build's: other rules (the key's), whose guards refuse a
/// computed text holding `word` that the build before showed.
pub(crate) fn new_build(word: &str) {
    BUILD.with(|slot| *slot.borrow_mut() = Some(word.to_owned()));
}

/// Make this thread's passes this build's again: [`new_build`] undone.
pub(crate) fn this_build() {
    BUILD.with(|slot| *slot.borrow_mut() = None);
}

/// The word this thread's later build refuses, where it is one.
pub(crate) fn build() -> Option<String> {
    BUILD.with(|slot| slot.borrow().clone())
}

/// Make this thread's passes read as if the session's members bound this Mac's actor to the
/// machine `machine`: a pass's `me` ([`super::materialize`]'s `=here`) becomes its uuid.
pub(crate) fn bound_to(machine: [u8; 16]) {
    BOUND.with(|slot| slot.set(Some(machine)));
}

/// The machine this thread's members bind this Mac's actor to, where a test said.
pub(crate) fn bound() -> Option<[u8; 16]> {
    BOUND.with(Cell::get)
}

/// Make this thread's passes read as if the session's members no longer held the member whose
/// binding names `machine`: it leaves a pass's current members (a view's `Seats::current`),
/// while its records and its facts row stand, as a fenced predecessor's do.
pub(crate) fn leaves(machine: [u8; 16]) {
    LEFT.with(|slot| slot.set(Some(machine)));
}

/// The machine whose member this thread's members no longer hold, where a test said.
pub(crate) fn left() -> Option<[u8; 16]> {
    LEFT.with(Cell::get)
}

/// Make this thread's passes meet a session roster that does not read: `Session::members`
/// refusing, with an error that names no busy holder.
pub(crate) fn roster_unread() {
    UNREAD.with(|slot| slot.set(true));
}

/// Make this thread's passes read the session's roster again: [`roster_unread`] undone.
pub(crate) fn roster_reads() {
    UNREAD.with(|slot| slot.set(false));
}

/// The session's roster as this thread's pass reads it: `read`, or a refusal where a test said
/// the roster does not read.
pub(crate) fn roster<T>(read: crate::Result<T>) -> crate::Result<T> {
    if UNREAD.with(Cell::get) {
        return Err(crate::Error::new("a test's roster does not read"));
    }
    read
}

/// Run `step` once, on this thread, where the next table view read here has gathered the
/// records a roll carried and is about to authenticate them (`crate::att::carried`).
pub(crate) fn before_carried(step: impl FnOnce() + 'static) {
    CARRIED.with(|slot| *slot.borrow_mut() = Some(Box::new(step)));
}

pub(crate) fn carried_read() {
    let step = CARRIED.with(|slot| slot.borrow_mut().take());
    if let Some(step) = step {
        step();
    }
}

/// Called where this crate starts toward custody: every writer-generation selection
/// (`generation_until`, which each observing and writing open of a table begins with, and
/// the materializer's own) and every vault observation that may establish it
/// (`observed_bootstrap`).
pub(crate) fn custody() {
    CUSTODY.with(|count| count.set(count.get() + 1));
    let step = TOWARD.with(|slot| slot.borrow_mut().take());
    if let Some(step) = step {
        step();
    }
}

/// Run `step` once, where this thread next starts toward custody ([`custody`]), before that
/// open has waited for or read anything: a hold a test took ends exactly as a read turns to
/// its table.
pub(crate) fn toward_custody(step: impl FnOnce() + 'static) {
    TOWARD.with(|slot| *slot.borrow_mut() = Some(Box::new(step)));
}

/// How often this thread has started toward custody.
pub(crate) fn custody_opens() -> u64 {
    CUSTODY.with(Cell::get)
}

/// Run `step` once, after the next pass on this thread has closed custody and before it
/// records.
pub(crate) fn before_record(step: impl FnOnce() + 'static) {
    BETWEEN.with(|slot| *slot.borrow_mut() = Some(Box::new(step)));
}

/// Run `step` once, after the next pass on this thread has read its home's standing and
/// before it opens custody.
pub(crate) fn after_standing(step: impl FnOnce() + 'static) {
    STANDING.with(|slot| *slot.borrow_mut() = Some(Box::new(step)));
}

pub(crate) fn standing_read() {
    let step = STANDING.with(|slot| slot.borrow_mut().take());
    if let Some(step) = step {
        step();
    }
}

pub(crate) fn between() {
    let step = BETWEEN.with(|slot| slot.borrow_mut().take());
    if let Some(step) = step {
        step();
    }
}

/// An owner act for this crate's tests, as a person's verb makes one: aterm-db's one call of
/// the test seam past its clippy.toml row (one owner act).
pub(crate) fn owner() -> aterm_file::OwnerAct {
    #[expect(
        clippy::disallowed_methods,
        reason = "aterm-db's one test helper for an owner act"
    )]
    let act = aterm_file::owner_act_for_test("person", None);
    act.expect("an owner act")
}

/// The guard the outbox holds a cell of a column of type `ty` to, as a write verb stamps each
/// edit it queues, for a test outside `file` that queues an intent of a family no shared file
/// of this build names yet.
pub(crate) fn judged(ty: &crate::typed::Ty) -> aterm_file::outbox::Judged {
    super::write::judged(ty)
}

/// What `aterm file set <name> <row> <column> [<value>]` would queue over `paths`, for a test
/// outside `file`, which cannot name the private write verbs.
pub(crate) fn draft_set(
    paths: &aterm_secrets::client::Paths,
    name: &str,
    key: &str,
    column: &str,
    value: Option<&str>,
) -> Option<aterm_file::Draft> {
    super::write::draft_set(paths, name, key, &[(column, value)]).expect("a draft")
}
