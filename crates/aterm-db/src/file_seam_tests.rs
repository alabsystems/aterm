// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::{Cell, RefCell};

thread_local! {
    static CUSTODY: Cell<u64> = const { Cell::new(0) };
    static BETWEEN: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
    static STANDING: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
}

/// Called where this crate starts toward custody: every writer-generation selection
/// (`generation_until`, which each observing and writing open of a table begins with, and
/// the materializer's own) and every vault observation that may establish it
/// (`observed_bootstrap`).
pub(crate) fn custody() {
    CUSTODY.with(|count| count.set(count.get() + 1));
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

/// What `aterm file set <name> <row> <column> [<value>]` would queue over `paths`, for a test
/// outside `file`, which cannot name the private write verbs.
pub(crate) fn draft_set(
    paths: &aterm_secrets::client::Paths,
    name: &str,
    key: &str,
    column: &str,
    value: Option<&str>,
) -> Option<aterm_file::Draft> {
    super::write::draft_set(paths, name, key, column, value).expect("a draft")
}
