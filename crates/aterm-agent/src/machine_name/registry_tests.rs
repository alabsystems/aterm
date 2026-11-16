// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::RefCell;

use aterm_net::fleet_member::name_record::ByKind;
use aterm_types::machine_name::Nickname;

use super::{Written, refusal, row_nick, write_nick, write_nick_with};

const ROW: &str = "mc-0123456789abcdef";

fn heron() -> Nickname {
    Nickname::parse("Heron").expect("a nickname")
}

/// Each write `write_nick_with` asks for, and what it made of `answer`.
fn asked(
    row: Option<&str>,
    kind: ByKind,
    answer: Result<&str, &str>,
) -> (Written, Vec<[String; 3]>) {
    let calls = RefCell::new(Vec::new());
    let written = write_nick_with(row, &heron(), kind, |row, nick, by| {
        calls
            .borrow_mut()
            .push([row.to_owned(), nick.to_owned(), by.to_owned()]);
        answer.map(str::to_owned).map_err(str::to_owned)
    });
    (written, calls.into_inner())
}

#[test]
fn a_nickname_rename_queues_the_registry_row_write() {
    let (written, calls) = asked(Some(ROW), ByKind::Person, Ok("published 7f3a\n"));
    assert_eq!(
        calls,
        [[ROW.to_owned(), "Heron".to_owned(), "person".to_owned()]],
        "this Mac's own row gets the nickname, by its key, as a person's act"
    );
    assert_eq!(written, Written::Said("published 7f3a".to_owned()));
    assert_eq!(
        written.note().as_deref(),
        Some("lab registry: published 7f3a")
    );
}

#[test]
fn an_answer_of_two_lines_is_one_note() {
    let (written, _) = asked(
        Some(ROW),
        ByKind::Person,
        Ok("queued 01ab: the store is busy\nnote: the outbox was not flushed\n"),
    );
    assert_eq!(
        written.note().as_deref(),
        Some("lab registry: queued 01ab: the store is busy; note: the outbox was not flushed")
    );
}

#[test]
fn a_row_that_says_it_already_is_no_note() {
    let (written, calls) = asked(
        Some(ROW),
        ByKind::Person,
        Ok("unchanged: lab/machines already says it\n"),
    );
    assert_eq!(calls.len(), 1);
    assert_eq!(written, Written::Unchanged);
    assert_eq!(written.note(), None);
}

#[test]
fn a_program_rename_is_written_as_a_program_act() {
    for kind in [ByKind::Agent, ByKind::Auto] {
        let (_, calls) = asked(Some(ROW), kind, Ok("published 7f3a"));
        assert_eq!(calls[0][2], "program", "{kind:?}");
    }
}

#[test]
fn no_row_of_this_mac_writes_nothing() {
    let (written, calls) = asked(None, ByKind::Person, Ok("published 7f3a"));
    assert!(calls.is_empty(), "{calls:?}");
    assert_eq!(written, Written::Unbound);
    assert_eq!(written.note(), None);
}

#[test]
fn a_refused_write_says_why() {
    let (written, calls) = asked(
        Some(ROW),
        ByKind::Person,
        Err("lab/machines: row mc-0123456789abcdef is not published yet"),
    );
    assert_eq!(calls.len(), 1);
    assert_eq!(
        written.note().as_deref(),
        Some(
            "the lab registry's row was not written: lab/machines: row mc-0123456789abcdef is \
             not published yet"
        )
    );
}

/// A rename of a name kept anywhere but this account's live state writes no registry row, and a
/// read of one follows none ([`row_nick`]): on
/// 2026-11-15 a build without this rule let `name_cli`'s tests, renaming scratch names, queue
/// owner writes of the developer's own Mac's row, two of which were signed.
#[test]
fn a_rename_of_a_name_kept_elsewhere_writes_no_row() {
    // This binary's own state root is private (a test binary's scratch root): were it not, the
    // second case would name the live state and write the real row, so it must not run.
    assert!(aterm_types::dirs::runs_under_private_state_root());
    let scratch = aterm_tempfile::tempdir().expect("a scratch folder");
    for state in [
        scratch.path().to_path_buf(),
        aterm_types::dirs::state_dir().expect("a state folder"),
    ] {
        assert_eq!(
            write_nick(
                &state,
                &heron(),
                ByKind::Person,
                None,
                std::time::Duration::ZERO
            ),
            Written::Unbound,
            "{}",
            state.display()
        );
        assert_eq!(
            refusal(&state, &Nickname::parse("Password").expect("a nickname")),
            None,
            "{}",
            state.display()
        );
        assert_eq!(row_nick(&state), None, "{}", state.display());
    }
}
