// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use nle::Verdict::{Done, Leaves, Overtaken, Signs};

/// An entry that stands on its read ([`WaitingWrite::guarded`]) settles no conflict: over its
/// read value with another beside it, it is neither done nor signed, whether it writes that
/// value again or another, so a pass finds it overtaken. With no conflict, it is as a person's
/// write; and a person's write over the same conflict still stands over it.
#[test]
fn a_waiting_entry_that_stands_on_its_read_is_overtaken_by_a_conflict() {
    let read = Entry {
        name: "shared".to_owned(),
        value: SecretBytes::from(b"read".to_vec()),
        audience: Audience::Everyone,
        if_absent: false,
        anchor: None,
    };
    let amended = Entry {
        value: SecretBytes::from(b"amended".to_vec()),
        ..read.clone()
    };
    for (case, to, unconflicted) in [("again", &read, Done), ("amended", &amended, Signs)] {
        let person = WaitingWrite::member(to.clone(), Some(&read), false);
        let guarded = WaitingWrite {
            guarded: true,
            ..person.clone()
        };
        let verdict = |write: &WaitingWrite, conflicted| {
            nle::Verdict::of(
                false,
                write.done(Some(&read), conflicted),
                write.stands_over(Some(&read), conflicted),
                false,
            )
        };
        assert_eq!(verdict(&guarded, true), Overtaken, "{case}");
        assert_eq!(verdict(&guarded, false), unconflicted, "{case}");
        assert_eq!(verdict(&person, false), unconflicted, "{case}: CONTROL");
        assert_eq!(verdict(&person, true), unconflicted, "{case}: CONTROL");
    }
}

/// A member write's entries take one verdict in a pass: where one signs, one the log holds
/// already signs beside it; where one is overtaken, one that would sign is dropped with it; where
/// a pending set holds one back, the whole draft waits. A write of no draft (an edit's), and a
/// write of another draft, keep their own.
#[test]
fn a_member_writes_entries_take_one_verdict() {
    for (drafts, own, want) in [
        (
            [Some(0), Some(0), None],
            [Signs, Done, Done],
            [Signs, Signs, Done],
        ),
        (
            [Some(0), Some(0), None],
            [Signs, Overtaken, Signs],
            [Overtaken, Overtaken, Signs],
        ),
        (
            [Some(0), Some(0), Some(1)],
            [Done, Overtaken, Done],
            [Done, Overtaken, Done],
        ),
        (
            [Some(0), Some(0), Some(1)],
            [Signs, Leaves, Signs],
            [Leaves, Leaves, Signs],
        ),
        (
            [Some(0), Some(1), Some(1)],
            [Signs, Done, Done],
            [Signs, Done, Done],
        ),
        (
            [None, None, None],
            [Signs, Done, Overtaken],
            [Signs, Done, Overtaken],
        ),
    ] {
        assert_eq!(draft_verdicts(&drafts, &own), want, "{drafts:?} {own:?}");
    }
}

/// A write a pass drops only with its draft is dropped with the first write of that draft whose
/// own verdict is overtaken; one overtaken itself, one of no draft, and one whose draft holds no
/// write overtaken (another draft's being so) are dropped with none.
#[test]
fn a_write_dropped_with_its_draft_names_the_write_overtaken_itself() {
    for (drafts, own, want) in [
        (
            &[Some(0), Some(0), None][..],
            &[Signs, Overtaken, Signs][..],
            &[Some(1), None, None][..],
        ),
        (
            &[Some(0), Some(0), Some(0)],
            &[Overtaken, Signs, Overtaken],
            &[None, Some(0), None],
        ),
        (&[Some(0), Some(1)], &[Signs, Overtaken], &[None, None]),
    ] {
        let got: Vec<Option<usize>> = (0..own.len())
            .map(|at| dropped_with(drafts, own, at))
            .collect();
        assert_eq!(got, want, "{drafts:?} {own:?}");
    }
}

/// A journal line written before a record named the write its draft was dropped with reads back
/// as a write overtaken itself, and a record that names none is written in that same shape; one
/// that names it reads back with it, whether the log names the change or not. A line of no known
/// shape is skipped.
#[test]
fn an_overtaken_record_reads_back_and_reads_the_journal_main_wrote() {
    // The journal's record, not the verdict this file imports under the same name.
    use super::Overtaken;
    let hex = "ab".repeat(32);
    let by = Overtaker {
        record: [0xab; 32],
        label: "m7".to_owned(),
        authored_ms: 1_600,
        deleted: false,
    };
    let own = Overtaken {
        name: "token-meta".to_owned(),
        by: Some(by.clone()),
        with: None,
        dropped_ms: 1_700,
        shown: true,
    };
    let unnamed = Overtaken {
        by: None,
        shown: false,
        ..own.clone()
    };
    for (line, record) in [
        (format!("1700\t1\ttoken-meta\tput\t{hex}\tm7\t1600"), &own),
        ("1700\t0\ttoken-meta\t-".to_owned(), &unnamed),
    ] {
        assert_eq!(parse_overtaken(&line).as_ref(), Some(record), "{line}");
        assert_eq!(encode_overtaken(record), format!("{line}\n"), "{line}");
    }
    for with in [own.clone(), unnamed] {
        let with = Overtaken {
            name: "token".to_owned(),
            with: Some("token-meta".to_owned()),
            ..with
        };
        let line = encode_overtaken(&with);
        assert!(line.ends_with("\ttoken-meta\n"), "{line}");
        assert_eq!(parse_overtaken(line.trim_end()), Some(with), "{line}");
    }
    for bad in [
        format!("1700\t1\ttoken\tput\t{hex}\tm7\t1600\ttoken-meta\textra"),
        "1700\t0\ttoken\t-\ttoken-meta\textra".to_owned(),
        "1700\t0\ttoken\tput\ttoken-meta".to_owned(),
    ] {
        assert_eq!(parse_overtaken(&bad), None, "{bad}");
    }
}

/// A member write's number, and whether its entry stands on its read, are kept in the waiting
/// file and read back; an edit's write carries neither, and its line keeps the shape it had. A
/// number that is not one, or a word out of its place, is refused.
#[test]
fn a_waiting_writes_draft_reads_back() {
    let mut waiting =
        Waiting::of_puts(&[("shared-a", b"a"), ("shared-b", b"b"), ("shared-c", b"c")]);
    if let Some(write) = waiting.0.get_mut("shared-a") {
        write.draft = Some(7);
    }
    if let Some(write) = waiting.0.get_mut("shared-c") {
        write.draft = Some(7);
        write.guarded = true;
    }
    let text = waiting.encode();
    let back = Waiting::decode(text.as_slice()).expect("it reads back");
    assert_eq!(back.0["shared-a"].draft, Some(7));
    assert_eq!(back.0["shared-b"].draft, None);
    assert!(!back.0["shared-a"].guarded && !back.0["shared-b"].guarded);
    assert!(back.0["shared-c"].guarded, "it stands on its read");
    for name in ["shared-a", "shared-c"] {
        assert!(back.0[name].same(&waiting.0[name]), "{name}");
    }
    let lines: Vec<&[u8]> = text.split(|byte| *byte == b'\n').collect();
    assert!(lines[0].ends_with(b" d7"), "the member write's number");
    assert!(lines[2].ends_with(b" d7 read"), "and its guard");
    assert_eq!(
        lines[1].split(|byte| *byte == b' ').count(),
        7,
        "an edit's write keeps its shape"
    );
    for bad in [
        &b" dx"[..],
        b" d",
        b" 7",
        b" d7 d8",
        b" read d7",
        b" d7 read read",
        b" reads",
    ] {
        let mut line = lines[1].to_vec();
        line.extend_from_slice(bad);
        assert!(Waiting::decode(&line).is_err(), "{bad:?}");
    }
}
