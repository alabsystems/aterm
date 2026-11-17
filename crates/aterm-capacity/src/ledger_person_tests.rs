// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A person's claim ([`Need::person`]) on the wire.

use super::*;

fn record(need: Need) -> Record {
    Record {
        timing: crate::TimingEvidence::default(),
        accounting: crate::Accounting::Holder,
        pid: 42,
        start_ms: 7,
        laid_at: None,
        seq: 0,
        checkout: "/ck".to_owned(),
        need,
        held: None,
        credit: Vec::new(),
        wrote: None,
        next_ms: None,
    }
}

/// The head line of `record`, as JSON.
fn head(record: &Record) -> Map {
    match aterm_json::from_str::<Value>(record.head_line().trim_end()) {
        Ok(Value::Object(head)) => head,
        other => panic!("a head object: {other:?}"),
    }
}

/// A PERSON'S RECORD ADDS ONE KEY, `person: true`, AND KEEPS EVERY OTHER: its rank word stays
/// one of the three, so a reader that names no `person` key (an older aterm on the same Mac)
/// reads it as the ordinary waiter it was. It reads back marked, through a real ledger too.
///
/// NEGATIVE CONTROLS. An unmarked record's head has no `person` key; a record without the key
/// reads unmarked; a Release or Background record that says `person: true` reads unmarked, and a
/// ledger lays none so.
#[test]
fn a_persons_record_adds_one_key_and_keeps_every_other() {
    let need = Need::new("targo build").mem_gib(8).cores(4);
    let unmarked = record(need.clone());
    let marked = record(Need {
        person: true,
        ..need.clone()
    });
    let mut theirs = head(&marked);
    assert_eq!(theirs.remove("person"), Some(Value::from(true)));
    assert_eq!(theirs, head(&unmarked), "one key added, every other kept");
    assert_eq!(head(&marked).get("rank"), Some(&Value::from("ordinary")));
    assert!(!head(&unmarked).contains_key("person"));
    assert_eq!(
        Record::decode(&marked.head_line(), false).map(|r| r.need.person),
        Some(true)
    );
    assert_eq!(
        Record::decode(&unmarked.head_line(), false).map(|r| r.need.person),
        Some(false),
        "a record without the key reads unmarked"
    );

    // Only an Ordinary record is a person's, read or laid.
    for rank in [crate::Rank::Release, crate::Rank::Background] {
        let other = record(Need {
            rank,
            ..need.clone()
        });
        let mut line = head(&other);
        line.insert("person".to_owned(), Value::from(true));
        let decoded = Record::decode(&format!("{}\n", Value::Object(line)), false).expect("read");
        assert_eq!(decoded.need.rank, rank);
        assert!(!decoded.need.person, "{rank:?}");
    }

    let dir = aterm_tempfile::Builder::new()
        .prefix("cap-person-wire-")
        .tempdir()
        .expect("a scratch ledger");
    let ledger = Ledger::at(dir.path().join("ledger"));
    let me = std::process::id();
    let (_ordinary, _a) = ledger
        .lay_stand_in(
            &Need {
                person: true,
                ..need.clone()
            },
            1,
            me + 1,
        )
        .expect("laid");
    let (_release, _b) = ledger
        .lay_stand_in(
            &Need {
                person: true,
                rank: crate::Rank::Release,
                ..need
            },
            2,
            me + 2,
        )
        .expect("laid");
    let listed = ledger.list().expect("listed");
    assert_eq!(
        listed
            .iter()
            .map(|r| (r.need.rank, r.need.person))
            .collect::<Vec<_>>(),
        [(crate::Rank::Ordinary, true), (crate::Rank::Release, false)]
    );
}
