// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn deadline(off: u64, sid: &str) -> Deadline {
    Deadline {
        off,
        sid: sid.to_string(),
        at: 1_700_000_000_000 + off,
        dl: 60_000,
        to: Some("n-0123456789abcdef".to_string()),
    }
}

fn asked(off: u64) -> Asked {
    Asked {
        off,
        to: "h-andrew".to_string(),
    }
}

fn journal(frames: &[Frame]) -> Vec<u8> {
    encode_from(0, frames).expect("frames").0
}

/// Every kind of record reads back as written, the index is what they say in order, and a
/// rewrite from the index reads back to the same index, shorter.
#[test]
fn every_record_round_trips_and_a_rewrite_holds_the_same_index() {
    let frames = vec![
        Frame::Handed(3),
        Frame::Asked(asked(7)),
        Frame::Deadline(deadline(7, "s-a")),
        Frame::Asked(asked(9)),
        Frame::Deadline(Deadline {
            to: None,
            ..deadline(9, "s-b")
        }),
        Frame::DeadlineGone {
            off: 7,
            sid: "s-a".to_string(),
        },
        Frame::Handed(4),
        Frame::Took(Took {
            asked: 11,
            deadlines: u64::MAX,
        }),
    ];
    for frame in &frames {
        assert_eq!(
            Frame::of_record(frame.body().as_bytes(), b"").as_ref(),
            Some(frame)
        );
        assert!(frame.body().is_ascii(), "{frame:?}");
    }
    let bytes = journal(&frames);
    let read = read(&bytes);
    assert_eq!(read.end, journal::End::Clean);
    assert_eq!(read.next_step, frames.len() as u64);
    let expected = Index {
        asked: [(7, "h-andrew".to_string()), (9, "h-andrew".to_string())].into(),
        deadlines: [(
            9,
            Deadline {
                to: None,
                ..deadline(9, "s-b")
            },
        )]
        .into(),
        gone: [(7, "s-a".to_string())].into(),
        handed: Some(4),
        took: Some(Took {
            asked: 11,
            deadlines: u64::MAX,
        }),
        noted: false,
    };
    assert_eq!(read.index, expected);
    let (rewritten, steps) = read.index.encode().expect("rewrite");
    assert_eq!(
        steps, 6,
        "two recipients, a deadline, a settle, the hand-off, what it took"
    );
    let again = super::read(&rewritten);
    assert_eq!((again.index, again.end), (expected, journal::End::Clean));
    assert!(rewritten.len() < bytes.len());
}

/// A REWRITE KEEPS WHETHER ANYTHING WAS NOTED SINCE THE LAST HAND-OFF, which a table a build
/// before the journal writes later is read by ([`Index::word_of`]): a fact after the hand-off
/// reads back as noted, and CONTROL a hand-off after every fact reads back as not; a hand-off
/// forgets what was taken before it, since it replaced that table.
#[test]
fn a_rewrite_keeps_whether_anything_was_noted_since_the_last_hand_off() {
    let reread = |index: &Index| read(&index.encode().expect("rewrite").0).index;
    let mut index = Index::default();
    index.apply(Frame::Asked(asked(3)));
    index.apply(Frame::Took(Took {
        asked: 1,
        deadlines: 2,
    }));
    index.apply(Frame::Handed(1));
    assert_eq!(index.took, None, "a hand-off replaced the tables");
    assert!(!index.noted, "CONTROL: nothing after the hand-off");
    assert_eq!(reread(&index), index);
    for frame in [
        Frame::Asked(asked(4)),
        Frame::Deadline(deadline(4, "s-a")),
        Frame::DeadlineGone {
            off: 4,
            sid: "s-a".to_string(),
        },
    ] {
        let mut noted = index.clone();
        noted.apply(frame.clone());
        assert!(noted.noted, "{frame:?}");
        assert_eq!(reread(&noted), noted, "{frame:?}");
    }
}

/// A RECORD THE BODY CANNOT CARRY IS REFUSED, never written to be read back as another: a
/// session id or a recipient with a space, or one past printable ASCII.
#[test]
fn a_record_its_body_cannot_carry_is_refused() {
    for frame in [
        Frame::Asked(Asked {
            off: 1,
            to: "h-a b".to_string(),
        }),
        Frame::Asked(Asked {
            off: 1,
            to: "h-\u{e9}".to_string(),
        }),
        Frame::Deadline(deadline(1, "s-a b")),
        Frame::Deadline(deadline(1, "s-\u{e9}")),
        Frame::DeadlineGone {
            off: 1,
            sid: String::new(),
        },
    ] {
        let refused = journal::encode(0, &frame).expect_err("refused");
        assert_eq!(refused.kind(), io::ErrorKind::InvalidInput, "{frame:?}");
    }
    assert!(
        journal::encode(0, &Frame::Deadline(deadline(1, "s-a"))).is_ok(),
        "CONTROL"
    );
}

/// THE INDEX KEEPS THE TABLES' CAPS AS IT IS REPLAYED: the lowest offsets go first, as the
/// bridge's own tables keep them.
#[test]
fn the_index_trims_to_the_caps_on_replay() {
    let mut frames = Vec::new();
    for off in 0..ASKED_KEEP as u64 + 2 {
        frames.push(Frame::Asked(asked(off)));
    }
    for off in 0..DEADLINES_KEEP as u64 + 3 {
        frames.push(Frame::Deadline(deadline(off, "s-a")));
        frames.push(Frame::DeadlineGone {
            off: off + 100_000,
            sid: "s-a".to_string(),
        });
    }
    let index = read(&journal(&frames)).index;
    assert_eq!(index.asked.len(), ASKED_KEEP);
    assert_eq!(index.asked.first_key_value().map(|(off, _)| *off), Some(2));
    assert_eq!(index.deadlines.len(), DEADLINES_KEEP);
    assert_eq!(index.deadline_rows()[0].off, 3);
    assert_eq!(index.gone.len(), DEADLINES_KEEP);
    assert_eq!(
        index.gone.first_key_value().map(|(off, _)| *off),
        Some(100_003)
    );
}

/// A SETTLE NAMES ITS ASKER: it takes that asker's deadline out and is remembered; one of
/// another asker's at the offset leaves the deadline standing; a deadline armed again at the
/// offset forgets the settle.
#[test]
fn a_settle_takes_out_only_its_askers_deadline_and_is_remembered() {
    let mut index = Index::default();
    index.apply(Frame::Deadline(deadline(5, "s-a")));
    index.apply(Frame::DeadlineGone {
        off: 5,
        sid: "s-b".to_string(),
    });
    assert!(index.deadlines.contains_key(&5), "another asker's settle");
    index.apply(Frame::DeadlineGone {
        off: 5,
        sid: "s-a".to_string(),
    });
    assert!(index.deadlines.is_empty());
    assert!(index.settled(5, "s-a") && !index.settled(5, "s-b"));
    index.apply(Frame::Deadline(deadline(5, "s-a")));
    assert!(
        !index.settled(5, "s-a"),
        "the last record about the ask wins"
    );
}

/// A TABLE HANDED BACK READS TO A BUILD BEFORE THE JOURNAL ROW FOR ROW: its opening line is
/// no row of either table's parse (an older build skips it), and names the hand-off.
#[test]
fn a_handed_back_table_reads_row_for_row_to_a_build_before_the_journal() {
    let rows = [deadline(3, "s-a"), deadline(8, "s-b")];
    let text = legacy_text(7, rows.iter().map(Deadline::render));
    let parsed: Vec<Deadline> = text.lines().filter_map(Deadline::parse).collect();
    assert_eq!(parsed, rows, "what a build before the journal reads");
    assert_eq!(legacy_mark(&text), Some(7));
    let asked_text = legacy_text(7, [asked(3).render()]);
    let parsed: Vec<Asked> = asked_text.lines().filter_map(Asked::parse).collect();
    assert_eq!(parsed, [asked(3)]);
    assert_eq!(legacy_mark(&asked(3).render()), None, "a row is no mark");
    assert_eq!(legacy_mark(&deadline(3, "s-a").render()), None);
}

/// What a stop leaves in the index: asks 3 and 4 to their recipients, deadline 3 standing and
/// 4 settled, then hand-off 2.
fn handed_index() -> Index {
    let mut index = Index::default();
    index.apply(Frame::Asked(asked(3)));
    index.apply(Frame::Asked(asked(4)));
    index.apply(Frame::Deadline(deadline(3, "s-a")));
    index.apply(Frame::DeadlineGone {
        off: 4,
        sid: "s-a".to_string(),
    });
    index.apply(Frame::Handed(2));
    index
}

fn table(rows: &[Deadline]) -> String {
    rows.iter()
        .map(Deadline::render)
        .collect::<Vec<_>>()
        .join("\n")
}

/// A TABLE THAT NAMES THE JOURNAL'S OWN HAND-OFF, OR AN EARLIER ONE, SAYS NOTHING NEWER: its
/// write a power cut undid leaves the older one. CONTROL: one that names a later hand-off,
/// whose record a power cut lost, fills in.
#[test]
fn a_table_of_this_journals_hand_off_says_nothing_newer() {
    let rows = [deadline(8, "s-a")];
    for mark in [2, 1] {
        let mut index = handed_index();
        let text = legacy_text(mark, rows.iter().map(Deadline::render));
        assert!(!index.take_in(None, Some(&text), false), "hand-off {mark}");
        assert_eq!(index, handed_index(), "hand-off {mark}");
    }
    let mut index = handed_index();
    let text = legacy_text(3, rows.iter().map(Deadline::render));
    assert!(index.take_in(None, Some(&text), false), "CONTROL");
    assert_eq!(
        index.deadline_rows(),
        [deadline(3, "s-a"), deadline(8, "s-a")],
        "CONTROL: filled in, the journal's own kept"
    );
}

/// THE WORD OF A BUILD BEFORE THE JOURNAL THAT RAN AFTER A HAND-OFF REPLACES WHAT THE JOURNAL
/// SAYS OF THAT TABLE: what it settled or forgot stays gone (an empty table included), what
/// it asked stands, on an offset the journal knew on another log too, and no memory of a settle
/// stands in that ask's way. CONTROL: the same table, with a fact noted after the hand-off
/// (a kill, then that build), only fills in: it never saw what was noted, so that stays, and
/// a deadline noted settled is not armed again.
#[test]
fn a_table_of_an_older_build_after_a_hand_off_replaces_the_journals_word() {
    let moved = Deadline {
        to: Some("n-new".to_string()),
        ..deadline(4, "s-a")
    };
    let recipient = Asked {
        off: 4,
        to: "n-new".to_string(),
    };
    let mut index = handed_index();
    let text = table(std::slice::from_ref(&moved));
    assert!(index.take_in(Some(&recipient.render()), Some(&text), false));
    assert_eq!(
        index.deadline_rows(),
        std::slice::from_ref(&moved),
        "its row, alone"
    );
    assert_eq!(
        index.asked_rows(),
        std::slice::from_ref(&recipient),
        "its row, alone"
    );
    assert!(!index.settled(4, "s-a"), "no settle keeps its deadline out");
    assert!(!index.noted, "the taking-in is the journal's last word");

    let mut emptied = handed_index();
    assert!(emptied.take_in(Some(""), Some(""), false));
    assert!(emptied.asked.is_empty() && emptied.deadlines.is_empty());
    assert!(emptied.gone.is_empty());

    let mut one_table = handed_index();
    assert!(one_table.take_in(None, Some(""), false));
    assert_eq!(
        one_table.asked,
        handed_index().asked,
        "the table it left alone"
    );

    let mut control = handed_index();
    control.apply(Frame::Deadline(deadline(5, "s-a")));
    control.apply(Frame::Deadline(deadline(6, "s-a")));
    control.apply(Frame::DeadlineGone {
        off: 6,
        sid: "s-a".to_string(),
    });
    let theirs = table(&[
        Deadline {
            to: Some("n-new".to_string()),
            ..deadline(3, "s-a")
        },
        deadline(4, "s-a"),
        deadline(6, "s-a"),
        deadline(9, "s-a"),
    ]);
    let recipient = Asked {
        off: 3,
        to: "n-new".to_string(),
    };
    assert!(control.take_in(Some(&recipient.render()), Some(&theirs), false));
    assert_eq!(
        control
            .deadline_rows()
            .iter()
            .map(|d| d.off)
            .collect::<Vec<_>>(),
        [3, 5, 9],
        "CONTROL: its rows taken, the noted one kept, no settled one armed"
    );
    assert_eq!(
        control.deadlines.get(&3).and_then(|d| d.to.as_deref()),
        Some("n-new"),
        "CONTROL: its row is the later word at the offset"
    );
    assert_eq!(control.asked.get(&3).map(String::as_str), Some("n-new"));
    assert!(control.asked.contains_key(&4), "CONTROL: nothing dropped");
}

/// A TABLE IS TAKEN IN ONCE, across a rewrite too: its digest is recorded, so a deadline it
/// held that was then settled stays settled however long ago the settle aged out of the
/// index. CONTROL: another table, at the same point, is read; and a hand-off forgets the
/// digest, so the same text a build writes after that hand-off is read as its word.
#[test]
fn a_table_is_taken_in_once_and_its_settle_never_ages_out() {
    let text = table(&[deadline(3, "s-a")]);
    let mut index = Index::default();
    assert!(index.take_in(None, Some(&text), false));
    assert_eq!(index.deadline_rows(), [deadline(3, "s-a")]);
    index.apply(Frame::DeadlineGone {
        off: 3,
        sid: "s-a".to_string(),
    });
    for off in 0..=DEADLINES_KEEP as u64 {
        index.apply(Frame::DeadlineGone {
            off: 1_000 + off,
            sid: "s-a".to_string(),
        });
    }
    assert!(!index.settled(3, "s-a"), "the settle aged out");
    let mut replayed = read(&index.encode().expect("rewrite").0).index;
    assert_eq!(replayed, index, "the record of what it took is rewritten");
    assert!(!replayed.take_in(None, Some(&text), false), "taken once");
    assert!(replayed.deadlines.is_empty(), "armed nowhere");

    let other = table(&[deadline(5, "s-a")]);
    assert!(replayed.take_in(None, Some(&other), false), "CONTROL");
    assert_eq!(replayed.deadline_rows(), [deadline(5, "s-a")]);
    replayed.apply(Frame::Handed(1));
    assert!(
        replayed.take_in(None, Some(&other), false),
        "CONTROL: after a hand-off that text is that build's own word"
    );
}

/// DAMAGED RECORDS TRUST NO LINE: a table fills in what they lost, though it names the hand-off
/// the journal's salvage reads and though its digest is the one the salvage says was taken;
/// and a deadline the salvage remembers settled is still not armed. CONTROL: undamaged, the
/// same table adds nothing.
#[test]
fn damaged_records_take_back_what_a_table_holds_of_them() {
    let mut salvaged = Index::default();
    salvaged.apply(Frame::Deadline(deadline(4, "s-a")));
    salvaged.apply(Frame::DeadlineGone {
        off: 5,
        sid: "s-a".to_string(),
    });
    salvaged.apply(Frame::Handed(2));
    let rows = [deadline(3, "s-a"), deadline(4, "s-a"), deadline(5, "s-a")];
    let text = legacy_text(2, rows.iter().map(Deadline::render));

    let mut control = salvaged.clone();
    assert!(!control.take_in(None, Some(&text), false), "CONTROL");
    assert_eq!(control, salvaged, "CONTROL");

    let mut index = salvaged.clone();
    assert!(index.take_in(None, Some(&text), true));
    assert_eq!(
        index.deadline_rows(),
        [deadline(3, "s-a"), deadline(4, "s-a")],
        "the lost row back, the settled one not armed"
    );
    let mut again = index.clone();
    assert!(again.take_in(None, Some(&text), true), "still damaged");
    assert!(
        !index.take_in(None, Some(&text), false),
        "a rewritten journal holds what it took"
    );
}
