// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::collections::BTreeMap;

use aterm_agent::machine_name::{ensure, rename_preview};
use aterm_net::fleet_member::name_record::FLOOR_MS;
use aterm_secrets::client::{LabLabels, LabelHold};

use super::*;

/// The name a read found.
fn read(name: &str, word: &str) -> Held {
    Held {
        name: name.to_owned(),
        nick: word.to_owned(),
        word: word.to_owned(),
        read: true,
        waits: false,
    }
}

/// The window's view, before the press's read lands.
fn shown(name: &str, word: &str) -> Held {
    Held {
        read: false,
        ..read(name, word)
    }
}

/// A field the person typed `text` into.
fn typed(text: &str) -> Field {
    Field::open(Some(text), "")
}

/// A field left at the held name's `text`.
fn left(text: &str) -> Field {
    Field::open(None, text)
}

fn num(n: u32) -> Number {
    Number::new(n).expect("a number")
}

fn name(n: u32, nick: Option<&str>) -> RenameTo {
    RenameTo::Name(NameTo {
        num: num(n),
        nick: nick.map(|nick| Nickname::parse(nick).expect("a nickname")),
    })
}

fn nick(word: &str) -> RenameTo {
    RenameTo::Nick(Nickname::parse(word).expect("a nickname"))
}

#[test]
fn the_fields_keep_nickname_case_and_an_emptied_field_keeps_what_is_held() {
    let held = read("m184", "Linnet");
    assert_eq!(
        rename_to(&typed("  m86 "), &typed(" Kestrel "), &held),
        Ok(name(86, Some("Kestrel")))
    );
    for kept in [typed(""), typed("  "), typed("Linnet"), left("Linnet")] {
        assert_eq!(
            rename_to(&typed("86"), &kept, &held),
            Ok(name(86, None)),
            "{kept:?}"
        );
    }
    assert_eq!(
        rename_to(&typed("m86"), &typed("Blue Heron"), &held),
        Ok(name(86, Some("Blue Heron")))
    );
    for number in [
        left("m184"),
        typed("m184"),
        typed("184"),
        typed(""),
        typed("  "),
    ] {
        assert_eq!(
            rename_to(&number, &typed("Puffin"), &held),
            Ok(nick("Puffin")),
            "a nickname alone keeps the number: {number:?}"
        );
    }
}

#[test]
fn the_fields_refuse_what_is_no_name_and_ask_nothing_of_what_was_left_alone() {
    let held = read("m184", "Linnet");
    for number in ["m0", "m086", "x", "m1234567"] {
        assert!(
            rename_to(&typed(number), &typed("Linnet2"), &held).is_err(),
            "{number:?}"
        );
    }
    for word in ["m8", "--bad", "9lives"] {
        assert!(
            rename_to(&typed("m86"), &typed(word), &held).is_err(),
            "{word:?}"
        );
    }
    for (number, word) in [
        (left("m184"), left("Linnet")),
        (typed("m184"), typed("Linnet")),
        (typed("184"), left("Linnet")),
        (typed(" m184 "), typed("")),
        (typed(""), left("Linnet")),
        (typed(" "), typed("")),
    ] {
        assert_eq!(
            rename_to(&number, &word, &held),
            Ok(RenameTo::Keep),
            "{number:?} {word:?}"
        );
    }
    // An unnumbered Mac's nickname alone is set as Change Nickname… sets it.
    let unnumbered = read("m0", "Linnet");
    for number in [left("m0"), typed("m0")] {
        assert_eq!(
            rename_to(&number, &left("Linnet"), &unnumbered),
            Ok(RenameTo::Keep)
        );
        assert_eq!(
            rename_to(&number, &typed("Puffin"), &unnumbered),
            Ok(nick("Puffin"))
        );
    }
    let (_root, preview) = preview();
    let snapshot = Snapshot::Read(Box::new(preview));
    assert_eq!(
        line(&snapshot, &unnumbered, &left("m0"), &left("Linnet")),
        Line {
            text: "This Mac has no number yet. Type one, such as m8.".to_owned(),
            renames: true,
        }
    );
    assert_eq!(
        line(&snapshot, &unnumbered, &left("m0"), &typed("Puffin")),
        Line {
            text: "This Mac has no number yet; Rename makes its nickname Puffin.".to_owned(),
            renames: true,
        }
    );
    assert_eq!(
        line(
            &Snapshot::Reading,
            &shown("m0", "Linnet"),
            &left("m0"),
            &left("Linnet")
        )
        .text,
        "Reading this Mac's name\u{2026}",
        "an untouched form says nothing of a view the read may outdate"
    );
    assert_eq!(
        line(
            &Snapshot::Reading,
            &shown("m0", "Linnet"),
            &left("m0"),
            &typed("Puffin")
        )
        .text,
        "Rename keeps this Mac's number and makes its nickname Puffin.",
        "nor does a nickname alone"
    );
}

/// Each field the person has not typed into follows a read that found another name, whatever
/// the other field holds; a typed field, a failed set's words, and a read that agrees stay, and a
/// failed set's words are judged against the read from then on.
#[test]
fn each_field_follows_the_read_on_its_own() {
    let mut number = left("m7");
    assert!(number.follow("m87"));
    assert_eq!((number.text(), number.typed()), ("m87", None));
    let mut touched = Field::at("m8".to_owned(), Some("m7".to_owned()));
    assert!(!touched.follow("m87"));
    assert_eq!(touched.typed(), Some("m8"));
    let mut failed = Field::open(Some("m7"), "m7");
    assert!(
        !failed.follow("m87"),
        "a failed set's words stay the person's"
    );
    assert_eq!(
        (failed.text(), failed.opened(), failed.typed()),
        ("m7", Some("m87"), Some("m7"))
    );
    let mut landed = Field::open(Some("m87"), "m7");
    assert!(!landed.follow("m87"));
    assert_eq!(
        landed.typed(),
        None,
        "a failed set's words the read found ask for nothing"
    );
    let mut agrees = left("m7");
    assert!(!agrees.follow("m7"));
    // A nickname set a moment before lands with the read while the person types a number: the
    // untouched Nickname follows it, so Rename keeps it.
    let mut number = Field::at("m87".to_owned(), Some("m7".to_owned()));
    let mut word = left("Heron");
    let fresh = read("m7", "Kestrel");
    assert!(!number.follow(&fresh.name));
    assert!(word.follow(&fresh.word));
    assert_eq!(rename_to(&number, &word, &fresh), Ok(name(87, None)));
    // A number set a moment before lands while the person types a nickname: the untouched
    // Number follows it, so Rename sets the nickname alone.
    let mut number = left("m7");
    let word = Field::at("Puffin".to_owned(), Some("Heron".to_owned()));
    let fresh = read("m87", "Heron");
    assert!(number.follow(&fresh.name));
    assert_eq!(rename_to(&number, &word, &fresh), Ok(nick("Puffin")));
}

/// A field typed into before the read lands is judged against the read from then on: typing the
/// window's outdated text back asks for it, and typing the read's own keeps it, in either
/// spelling of a number.
#[test]
fn a_typed_field_is_judged_against_the_read_once_it_lands() {
    let fresh = read("m90", "Marigold");
    for back in ["m86", "86"] {
        let mut number = Field::at("m8".to_owned(), Some("m86".to_owned()));
        assert!(!number.follow(&fresh.name), "a typed field keeps its text");
        let number = Field::at(back.to_owned(), number.opened().map(str::to_owned));
        assert_eq!(number.typed(), Some(back));
        assert_eq!(
            rename_to(&number, &left("Marigold"), &fresh),
            Ok(name(86, None)),
            "{back}"
        );
    }
    let mut number = Field::at("m9".to_owned(), Some("m86".to_owned()));
    number.follow(&fresh.name);
    let number = Field::at("m90".to_owned(), number.opened().map(str::to_owned));
    assert_eq!(number.typed(), None, "the read's own text is no request");
    let mut word = Field::at("Hero".to_owned(), Some("Heron".to_owned()));
    word.follow(&fresh.word);
    let word = Field::at("Heron".to_owned(), word.opened().map(str::to_owned));
    assert_eq!(
        rename_to(&left("m90"), &word, &fresh),
        Ok(nick("Heron")),
        "the outdated nickname typed back is asked for"
    );
}

/// A number a migration still waits on holds no claim: its Number field opens empty, leaving it
/// asks for nothing, and typing the shown number claims it after the read's checks.
#[test]
fn a_number_that_holds_no_claim_is_claimed_only_when_typed() {
    let root = aterm_tempfile::tempdir().expect("a state folder");
    let now = FLOOR_MS + 300 * 86_400_000;
    ensure(root.path(), || Ok(Some("m7".to_owned())), |_| false, now).expect("a name");
    aterm_agent::machine_name::file::replace(root.path(), |name| {
        name.migration = Some(aterm_agent::machine_name::file::Migration::Waiting);
        Ok(())
    })
    .expect("a waiting number");
    let preview =
        rename_preview(root.path(), || Ok(LabLabels::default()), || Ok(None), now).expect("a read");
    let held = Held::of_read(&preview);
    assert!(held.waits);
    assert_eq!((held.name.as_str(), held.number_text()), ("m7", ""));
    let snapshot = Snapshot::Read(Box::new(preview));
    let word = held.word.clone();
    let open = Field::open(None, held.number_text());
    assert_eq!(rename_to(&open, &left(&word), &held), Ok(RenameTo::Keep));
    assert_eq!(
        line(&snapshot, &held, &open, &left(&word)),
        Line {
            text: "This Mac shows m7, which it has not claimed in this fleet yet. Type m7 to \
                   claim it, or another number."
                .to_owned(),
            renames: true,
        }
    );
    for typed_number in ["m7", "7"] {
        assert_eq!(
            rename_to(&typed(typed_number), &left(&word), &held),
            Ok(name(7, None))
        );
    }
    assert_eq!(
        line(&snapshot, &held, &typed("m7"), &left(&word)),
        Line {
            text: "Rename claims m7, which this Mac shows but has not claimed yet.".to_owned(),
            renames: true,
        }
    );
    assert_eq!(
        rename_to(&typed("m87"), &left(&word), &held),
        Ok(name(87, None))
    );
    assert_eq!(
        line(&snapshot, &held, &typed("m87"), &left(&word)).text,
        "m87 is free.",
        "another number is judged as any move is"
    );
}

/// A nickname another Mac carries too is said beside a nickname alone and beside a number, and
/// leaves Rename available: two Macs may share one.
#[test]
fn a_shared_nickname_is_said_and_refuses_nothing() {
    let (_root, preview) = preview();
    let otter = Nickname::parse("Otter").expect("a nickname");
    let held = Held::of_read(&preview);
    let snapshot = Snapshot::Read(Box::new(preview.with_taken_for_test(vec![otter])));
    for number in [left("m7"), typed("m87")] {
        let said = line(&snapshot, &held, &number, &typed("otter"));
        assert!(said.renames, "{said:?}");
        assert!(
            said.text.ends_with(" Another Mac is called otter too."),
            "{said:?}"
        );
    }
    let alone = line(&snapshot, &held, &typed("m87"), &typed("Kestrel"));
    assert_eq!(alone.text, "m87 is free.");
}

/// Before the read lands a typed field is asked for as typed, since a change pressed before may
/// still move the name the window shows; once it lands, typed text that equals it keeps it.
#[test]
fn before_the_read_lands_a_typed_field_is_asked_for_as_typed() {
    let stale = shown("m7", "Linnet");
    assert_eq!(
        rename_to(&left("m7"), &left("Linnet"), &stale),
        Ok(RenameTo::Keep)
    );
    assert_eq!(
        rename_to(&typed("7"), &left("Linnet"), &stale),
        Ok(name(7, None))
    );
    assert_eq!(
        rename_to(&left("m7"), &typed("Linnet"), &stale),
        Ok(nick("Linnet"))
    );
    let fresh = read("m87", "Linnet");
    assert_eq!(
        rename_to(&typed("m7"), &left("Linnet"), &fresh),
        Ok(name(7, None)),
        "a rename back"
    );
    assert_eq!(
        rename_to(&typed("87"), &typed("Linnet"), &fresh),
        Ok(RenameTo::Keep)
    );
}

/// A preview of a state named `m7`, whose vault holds `m20` for another Mac.
fn preview() -> (aterm_tempfile::TempDir, RenamePreview) {
    let root = aterm_tempfile::tempdir().expect("a state folder");
    let now = FLOOR_MS + 300 * 86_400_000;
    ensure(root.path(), || Ok(Some("m7".to_owned())), |_| false, now).expect("a name");
    let lab = LabLabels {
        held: BTreeMap::from([(num(20), LabelHold::Label)]),
        ..LabLabels::default()
    };
    let preview = rename_preview(root.path(), || Ok(lab), || Ok(None), now).expect("a read");
    (root, preview)
}

/// The line's Rename follows the fields and the read: a field that is no name, or a number the
/// read refuses, leaves Rename unavailable with that refusal's words; before the read lands and
/// after one that failed, Rename stays available, since the set checks the number itself.
#[test]
fn rename_is_unavailable_only_while_the_fields_or_the_read_refuse() {
    let (_root, preview) = preview();
    let fresh = Held::of_read(&preview);
    assert_eq!(
        (fresh.name.as_str(), fresh.read, fresh.waits),
        ("m7", true, false)
    );
    let stale = shown("m7", &fresh.word);
    let word = fresh.word.clone();
    let read = Snapshot::Read(Box::new(preview));
    let reading = Snapshot::Reading;
    let unread = Snapshot::Unread("the books are unread".to_owned());
    for (snapshot, held) in [(&read, &fresh), (&reading, &stale), (&unread, &stale)] {
        let refused = line(snapshot, held, &typed("m086"), &left(&word));
        assert!(!refused.renames, "{snapshot:?}");
        assert_eq!(
            Err(refused.text),
            rename_to(&typed("m086"), &left(&word), held),
            "the field's own refusal"
        );
        assert!(
            line(snapshot, held, &left("m7"), &left(&word)).renames,
            "untouched"
        );
        assert!(
            line(snapshot, held, &typed("m87"), &typed("")).renames,
            "free: {snapshot:?}"
        );
    }
    assert_eq!(
        line(&read, &fresh, &left("m7"), &left(&word)).text,
        format!(
            "This Mac is m7 {}. Type another number or nickname.",
            fresh.nick
        )
    );
    let held_by_vault = line(&read, &fresh, &typed("m20"), &left(&word));
    assert!(!held_by_vault.renames);
    let Snapshot::Read(preview) = &read else {
        unreachable!("a read")
    };
    let why = preview.judge(num(20)).expect_err("the vault holds m20");
    assert!(held_by_vault.text.starts_with(&why), "{held_by_vault:?}");
    assert!(line(&reading, &stale, &typed("m20"), &left(&word)).renames);
    assert!(line(&unread, &stale, &typed("m20"), &left(&word)).renames);
    assert_eq!(
        line(&unread, &stale, &left("m7"), &left(&word)).text,
        "This Mac's name could not be read here (the books are unread). Type a number or \
         nickname; Rename checks it.",
        "an untouched form says nothing of a view the failed read could not check"
    );
}

/// A read whose vault labels failed leaves a moving number unchecked, not refused: the set reads
/// them again. A kept number needs none.
#[test]
fn a_number_the_vault_could_not_judge_is_left_for_the_set_to_check() {
    let root = aterm_tempfile::tempdir().expect("a state folder");
    let now = FLOOR_MS + 300 * 86_400_000;
    ensure(root.path(), || Ok(Some("m7".to_owned())), |_| false, now).expect("a name");
    let preview = rename_preview(
        root.path(),
        || Err(std::io::Error::other("the vault is busy")),
        || Ok(None),
        now,
    )
    .expect("a read");
    let held = Held::of_read(&preview);
    let word = held.word.clone();
    let snapshot = Snapshot::Read(Box::new(preview));
    assert_eq!(
        line(&snapshot, &held, &typed("m87"), &left(&word)),
        Line {
            text: "m87 could not be checked here (the vault is busy); Rename checks it.".to_owned(),
            renames: true,
        }
    );
    assert_eq!(
        line(&snapshot, &held, &left("m7"), &typed("Kestrel")).text,
        "Rename keeps this Mac's number and makes its nickname Kestrel."
    );
}

/// The press slot keeps a read only for the press it holds: one for a press a later one
/// superseded, or for one that ended, is dropped, and ending an old press leaves the new one's.
#[test]
fn a_read_reaches_only_the_press_that_asked_for_it() {
    let mut slot = Some((2, Snapshot::Reading));
    assert!(!keep_read(&mut slot, 1, Snapshot::Unread("old".to_owned())));
    assert!(matches!(slot, Some((2, Snapshot::Reading))));
    assert!(keep_read(&mut slot, 2, Snapshot::Unread("new".to_owned())));
    assert!(matches!(&slot, Some((2, Snapshot::Unread(why))) if why == "new"));
    close(&mut slot, 1);
    assert!(
        slot.is_some(),
        "an earlier press's end leaves the newer press"
    );
    close(&mut slot, 2);
    assert!(slot.is_none());
    assert!(
        !keep_read(&mut slot, 2, Snapshot::Reading),
        "a read after its press ended"
    );
    assert!(slot.is_none());
}

#[test]
fn a_refusal_reads_as_a_sentence_without_recasing_a_number() {
    assert_eq!(sentence("the vault is busy"), "The vault is busy.");
    assert_eq!(sentence("m20 is held."), "m20 is held.");
}
