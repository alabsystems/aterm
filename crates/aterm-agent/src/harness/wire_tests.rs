// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the control-socket adapter.
//!
//! What is testable WITHOUT a live instance is the parsing and the ledger
//! seam; the two-connection park is exercised by `aterm harness watch`
//! against a real aterm (design §5.7), and this file does not pretend
//! otherwise.

use super::*;

#[test]
fn a_field_is_read_off_a_reply_line_and_a_dash_reads_as_absent() {
    let row = "local s-7f3 - live title meta=- nonce=0123456789abcdef0123456789abcdef window=0";
    assert_eq!(
        field_of(row, "nonce").as_deref(),
        Some("0123456789abcdef0123456789abcdef")
    );
    assert_eq!(field_of(row, "window").as_deref(), Some("0"));
    // NEGATIVE CONTROLS: `-` is "the server could not say", an absent key is
    // absent, and a PREFIX of a key is not that key.
    assert_eq!(field_of(row, "meta"), None);
    assert_eq!(field_of(row, "driving"), None);
    assert_eq!(field_of(row, "nonc"), None);
}

#[test]
fn the_two_actuator_rings_are_appendable_under_a_state_directory() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-harness-wire-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let first = super::super::cli::append_to(&dir, watch::RING_RECOVERY, "{\"id\":1}")
        .expect("the recovery ring must be appendable — pump journals before it acts");
    let second = super::super::cli::append_to(&dir, watch::RING_ACTUATION, "{\"id\":2}")
        .expect("the actuation ring carries refusal verdicts");
    assert!(first >= 1 && second >= 1);
    // NEGATIVE CONTROL: a name no ring answers to is refused, not created.
    assert!(super::super::cli::append_to(&dir, "no-such-ring", "{}").is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The custody fence catches a person READING and a person SELECTING.
///
/// It used to read `owner=user` only — `display_offset > 0`, i.e. scrolled
/// back — so a human holding a live selection at the TAIL got an L3 typed
/// act while the comment beside the fence said it meant "reading or
/// selecting". The `changed=` conjunct that sat beside it was a no-op:
/// `cmd_custody` prints `changed=none` when nothing moved, and `field_of`
/// filters only `-` and the empty string.
#[test]
fn the_custody_fence_catches_reading_and_selecting_and_nothing_else() {
    // MEASURED shape (`aterm-gui/src/control_query.rs::cmd_custody`).
    let tail = "OK last=none event=- changed=none took_selection=none offset=0 owner=tail \
                selection=no scrollback=1200";
    let scrolled = "OK last=drag event=- changed=drag took_selection=drag offset=40 owner=user \
                    selection=no scrollback=1200";
    let selecting = "OK last=drag event=- changed=drag took_selection=drag offset=0 owner=tail \
                     selection=yes scrollback=1200";
    assert!(custody_is_the_person(scrolled), "a person is reading");
    assert!(custody_is_the_person(selecting), "a person is selecting");
    // NEGATIVE CONTROL: at the tail with no selection nobody is in custody,
    // and `changed=none` — the value the old conjunct could never reject —
    // must not make it one.
    assert!(!custody_is_the_person(tail));
    assert!(
        field_of(tail, "changed").is_some(),
        "changed=none is a value"
    );
    // NEGATIVE CONTROL: a reply that carries neither key says nothing.
    assert!(!custody_is_the_person("OK"));
}
