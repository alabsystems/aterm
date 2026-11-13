// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn appends_preserve_interrupted_bytes_and_start_separate_records() {
    for prefix in [b"".as_slice(), b"{}\n", b"{}", b"{\"broken\":", b"\xff"] {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let path = tmp.path().join("journal.jsonl");
        std::fs::write(&path, prefix).unwrap();
        let mut warn = Vec::new();
        let mut journal = Journal::open(Some(&path), None, &mut warn);
        assert!(journal.append_raw("{\"id\":1}", &mut warn));
        let recorded = journal.record(2, "TIMEOUT", &mut warn).unwrap();
        let mut expected = prefix.to_vec();
        if !prefix.is_empty() && !prefix.ends_with(b"\n") {
            expected.push(b'\n');
        }
        expected.extend_from_slice(b"{\"id\":1}\n");
        expected.extend_from_slice(recorded.as_bytes());
        assert_eq!(std::fs::read(&path).unwrap(), expected);
        assert!(warn.is_empty());

        // A later interrupted append is checked too, not just the constructor's tail.
        std::fs::write(&path, b"{\"later\":").unwrap();
        assert!(journal.append_raw("{\"id\":3}", &mut warn));
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"later\":\n{\"id\":3}\n");
    }
}

#[test]
fn a_contended_journal_keeps_its_bytes_and_can_retry_after_unlock() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal.jsonl");
    let mut warn = Vec::new();
    let mut journal = Journal::open(Some(&path), None, &mut warn);
    let held = aterm_uds::line_log::lock(&path).unwrap();
    assert!(!journal.append_raw("{\"id\":1}", &mut warn));
    assert!(std::fs::read(&path).unwrap().is_empty());
    assert!(!warn.is_empty());
    drop(held);
    assert!(journal.append_raw("{\"id\":2}", &mut warn));
    assert_eq!(std::fs::read(&path).unwrap(), b"{\"id\":2}\n");
}

#[test]
fn a_journal_recovers_after_its_initial_open_failed() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let parent = tmp.path().join("missing");
    let path = parent.join("journal.jsonl");
    let mut journal = Journal::open(Some(&path), None, &mut Vec::new());
    assert!(!journal.append_raw("{}", &mut Vec::new()));
    std::fs::create_dir(&parent).unwrap();
    assert!(journal.append_raw("{}", &mut Vec::new()));
    assert_eq!(std::fs::read(&path).unwrap(), b"{}\n");

    let named_path = parent.join("named.jsonl");
    let mut named = Journal::named(Some(&named_path), None);
    assert!(named.record(1, "TIMEOUT", &mut Vec::new()).is_some());
    assert!(!named.append_raw("{}", &mut Vec::new()));
    assert!(!named_path.exists());
    assert!(!named_path.with_file_name("named.jsonl.lock").exists());
}

#[test]
fn independent_journal_writers_keep_every_acknowledged_row() {
    use std::sync::Barrier;
    let tmp = aterm_tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal.jsonl");
    std::fs::write(&path, b"{\"interrupted\":").unwrap();
    let start = Barrier::new(4);
    let mut accepted = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|id| {
                let path = &path;
                let start = &start;
                scope.spawn(move || {
                    let mut journal = Journal::open(Some(path), None, &mut Vec::new());
                    start.wait();
                    journal
                        .append_raw(&format!("{{\"id\":{id}}}"), &mut Vec::new())
                        .then_some(id)
                })
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|h| h.join().unwrap())
            .collect::<Vec<_>>()
    });
    let bytes = std::fs::read(&path).unwrap();
    let mut recorded: Vec<_> = bytes
        .split(|b| *b == b'\n')
        .filter_map(|line| aterm_json::from_slice::<aterm_json::Value>(line).ok())
        .map(|row| row["id"].as_u64().unwrap())
        .collect();
    accepted.sort_unstable();
    recorded.sort_unstable();
    assert!(!accepted.is_empty());
    assert_eq!(recorded, accepted);
}

#[cfg(unix)]
#[test]
fn journal_reopens_refuse_replaced_links_and_live_sockets() {
    use std::os::unix::{fs::symlink, net::UnixListener};
    let tmp = aterm_tempfile::Builder::new()
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .unwrap();
    let path = tmp.path().join("j");
    let target = tmp.path().join("target");
    std::fs::write(&target, b"keep").unwrap();
    let mut journal = Journal::open(Some(&path), None, &mut Vec::new());
    std::fs::remove_file(&path).unwrap();
    symlink(&target, &path).unwrap();
    assert!(!journal.append_raw("{}", &mut Vec::new()));
    assert_eq!(std::fs::read(&target).unwrap(), b"keep");
    std::fs::remove_file(&path).unwrap();
    let _listener = UnixListener::bind(&path).unwrap();
    assert!(!journal.append_raw("{}", &mut Vec::new()));
    std::fs::remove_file(&path).unwrap();
    std::fs::remove_file(path.with_file_name("j.lock")).unwrap();
    symlink(&target, path.with_file_name("j.lock")).unwrap();
    assert!(!journal.append_raw("{}", &mut Vec::new()));
    assert!(!path.exists());
    assert_eq!(std::fs::read(&target).unwrap(), b"keep");
}

/// Every kind of line the loops print, read into its kind, phase, `seq=`
/// and free-text tail: one row per arm of the reader.
#[test]
fn each_printed_line_is_read_into_its_fields() {
    let rec = |line: &str| JournalRecord::of_line(7, Some("@s-1"), line, None);
    for (line, kind, phase, seq, summary) in [
        (
            "EVENT idle seq=106 complete=1 rows=689 ⏺ Done.",
            "event",
            "idle",
            Some(106),
            "⏺ Done.",
        ),
        (
            "EVENT question seq=104 ⏺ Keep it?",
            "event",
            "question",
            Some(104),
            "⏺ Keep it?",
        ),
        (
            "EVENT compacted seq=102",
            "event",
            "compacted",
            Some(102),
            "",
        ),
        (
            "EVENT turn seq=106 report=5 rows=12 ⏺ Done.",
            "event",
            "turn",
            Some(106),
            "⏺ Done.",
        ),
        (
            "APPROVED seq=102 git log --oneline -5",
            "approved",
            "prompt",
            Some(102),
            "git log --oneline -5",
        ),
        (
            "DISMISSED survey seq=31",
            "dismissed",
            "survey",
            Some(31),
            "",
        ),
        (
            "RECONNECT await seq 7 failed: closed",
            "reconnect",
            "-",
            None,
            "await seq 7 failed: closed",
        ),
        (
            "RECONNECTED after 1834 ms",
            "reconnect",
            "-",
            None,
            "after 1834 ms",
        ),
        ("TIMEOUT", "timeout", "-", None, ""),
        ("EXIT session gone", "exit", "-", None, "session gone"),
        (
            "MAIL id=5 off=91 kind=report",
            "mail",
            "-",
            None,
            "id=5 off=91 kind=report",
        ),
        (
            "EXTEND until=2026-09-19T18:10:00Z",
            "extend",
            "-",
            None,
            "until=2026-09-19T18:10:00Z",
        ),
        (
            "ESCALATED seq=102 attention=OK",
            "escalated",
            "-",
            Some(102),
            "attention=OK",
        ),
        (
            "CLEARED seq=91 exception=spend",
            "cleared",
            "-",
            Some(91),
            "exception=spend",
        ),
        (
            "EXCEPTION seq=88 kind=spend when=later x",
            "exception",
            "-",
            Some(88),
            "kind=spend when=later x",
        ),
        (
            "CONTINUED seq=102 rule=usage-resume@v1 keep going",
            "continued",
            "usage-resume@v1",
            Some(102),
            "keep going",
        ),
        (
            "RESET seq=41 rule=limit-reset@v1 used (1 left)",
            "reset",
            "limit-reset@v1",
            Some(41),
            "used (1 left)",
        ),
        (
            "WAITING seq=9 until=2026-09-19T18:10:00Z overloaded",
            "waiting",
            "-",
            Some(9),
            "until=2026-09-19T18:10:00Z overloaded",
        ),
        (
            "HOLD seq=9 point nothing why=withheld",
            "hold",
            "point",
            Some(9),
            "nothing why=withheld",
        ),
        (
            "HOLD seq=12 background held why=act-untaken",
            "hold",
            "background",
            Some(12),
            "held why=act-untaken",
        ),
        (
            "OCCUPANCY seq=14 hold closed why=foreign held_s=7260",
            "occupancy",
            "hold",
            Some(14),
            "closed why=foreign held_s=7260",
        ),
        (
            "OCCUPANCY seq=15 rule=wait-nudge@v1 rev=builtin episode=00ff door=break would",
            "occupancy",
            "wait-nudge@v1",
            Some(15),
            "rev=builtin episode=00ff door=break would",
        ),
        (
            "PROBE deferred seq=102 text is typed",
            "probe",
            "deferred",
            Some(102),
            "text is typed",
        ),
        (
            "PROBE skipped gate=not-live",
            "probe",
            "skipped",
            None,
            "gate=not-live",
        ),
        (
            "PROBING seq=102 rule=usage-probe@v1 /usage",
            "other",
            "-",
            None,
            "PROBING seq=102 rule=usage-probe@v1 /usage",
        ),
        (
            "reset seq=41 rule=limit-reset@v1 unread",
            "other",
            "-",
            None,
            "reset seq=41 rule=limit-reset@v1 unread",
        ),
    ] {
        let r = rec(line);
        assert_eq!(
            (r.kind.as_str(), r.phase.as_str(), r.seq, r.summary.as_str()),
            (kind, phase, seq, summary),
            "{line}"
        );
        assert_eq!(r.sid.as_deref(), Some("s-1"));
    }
    let r = rec("EVENT idle seq=106 complete=1 rows=689 ⏺ Done.");
    assert_eq!(
        (r.complete, r.rows, r.report),
        (Some(true), Some(689), None)
    );
    let r = rec("EVENT turn seq=106 report=5 rows=12 ⏺ Done.");
    assert_eq!((r.complete, r.rows, r.report), (None, Some(12), Some(5)));
    for word in [
        "TYPED",
        "RESTARTED",
        "UNVERIFIED",
        "LIMITED",
        "UNPROVEN",
        "DIALOG",
        "DECLINED",
        "CHOSE",
        "PROBED",
    ] {
        let r = rec(&format!("{word} seq=5 rule=a-rule@v1 x"));
        assert_eq!(
            (r.kind, r.phase.as_str(), r.seq, r.summary.as_str()),
            (word.to_ascii_lowercase(), "a-rule@v1", Some(5), "x"),
            "{word}"
        );
    }
    for gate in crate::supervise::policy::turn_end::ProbeGate::ALL {
        let line = format!("SKIPPED seq=7 rule=usage-probe@v1 gate={}", gate.word());
        let r = rec(&line);
        assert_eq!(
            (r.kind.as_str(), r.phase.as_str(), r.seq, r.summary),
            (
                "skipped",
                "usage-probe@v1",
                Some(7),
                format!("gate={}", gate.word())
            ),
            "{line}"
        );
    }
}

/// Each [`Word`] is the kind of a line that begins with its printed form and of nothing
/// else, and its name reads back to it; a lower-case word or an unknown one is `other`.
#[test]
fn every_word_is_read_by_its_own_name() {
    let mut names: Vec<&str> = Word::ALL.iter().map(|w| w.name()).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), Word::ALL.len(), "two words share a name");
    for w in Word::ALL {
        let printed = w.printed();
        assert_eq!(Word::of_printed(&printed), Some(w), "{printed}");
        assert_eq!(Word::of_kind(w.name()), Some(w), "{w:?}");
        let r = JournalRecord::of_line(1, None, &format!("{printed} seq=1 x"), None);
        assert_eq!(r.kind, w.name(), "{printed}");
        assert_eq!(Word::of_printed(w.name()), None, "{w:?} in lower case");
    }
    assert_eq!(Word::Needs.printed(), NEEDS);
    assert_eq!(Word::of_printed("RECONNECTED"), Some(Word::Reconnect));
    for other in ["FROZEN", "ADOPTED", "PROBING", "", "Event"] {
        assert_eq!(Word::of_printed(other), None, "{other}");
        let r = JournalRecord::of_line(1, None, &format!("{other} seq=1 x"), None);
        assert_eq!(r.kind, "other", "{other}");
    }
    assert_eq!(Word::of_kind("other"), None);
}

/// [`log_words`] keeps each line's head as [`JournalRecord::of_line`] reads it and its
/// tail's length, never the tail: a tail holding a planted word and an equal-length tail of
/// other bytes give the same words, for every [`Word`] (each word's line shape is this
/// match's, so a new word does not compile until the test names it), every [`Noted`] word,
/// the shapes a real Claude Code run logged, and a line of no word.
#[test]
fn log_words_keeps_each_words_head_and_never_its_text() {
    const PLANT: &str = "QZP7WORD5";
    // The head the loop prints before a line's free text, and whether its grammar reads a
    // `seq=` there.
    let shape = |w: Word| -> (&'static str, bool) {
        match w {
            Word::Event => ("EVENT idle seq=7", true),
            Word::Dismissed => ("DISMISSED survey seq=7", true),
            Word::Mail => ("MAIL", false),
            Word::Approved => ("APPROVED seq=7", true),
            Word::Reconnect => ("RECONNECT", false),
            Word::Timeout => ("TIMEOUT", false),
            Word::Exit => ("EXIT", false),
            Word::Extend => ("EXTEND", false),
            Word::Escalated => ("ESCALATED seq=7", true),
            Word::Cleared => ("CLEARED seq=7", true),
            Word::Continued => ("CONTINUED seq=7 rule=usage-resume@v1", true),
            Word::Typed => ("TYPED seq=7 rule=a-rule@v1", true),
            Word::Restarted => ("RESTARTED seq=7 rule=a-rule@v1", true),
            Word::Waiting => ("WAITING seq=7", true),
            Word::Skipped => ("SKIPPED seq=7 rule=usage-probe@v1", true),
            Word::Unverified => ("UNVERIFIED seq=7 rule=a-rule@v1", true),
            Word::Limited => ("LIMITED seq=7 rule=a-rule@v1", true),
            Word::Unproven => ("UNPROVEN seq=7 rule=a-rule@v1", true),
            Word::Dialog => ("DIALOG seq=7 rule=a-rule@v1", true),
            Word::Declined => ("DECLINED seq=7 rule=a-rule@v1", true),
            Word::Chose => ("CHOSE seq=7 rule=answer-recommended@v1", true),
            Word::Exception => ("EXCEPTION seq=7", true),
            Word::Probed => ("PROBED seq=7 rule=usage-probe@v1", true),
            Word::Reset => ("RESET seq=7 rule=limit-reset@v1", true),
            Word::Needs => ("NEEDS seq=7", true),
            Word::Hold => ("HOLD seq=7 point", true),
            Word::Occupancy => ("OCCUPANCY seq=7 rule=wait-nudge@v1", true),
            Word::Probe => ("PROBE deferred seq=7", true),
        }
    };
    let mut rows: Vec<(String, bool)> = Word::ALL
        .into_iter()
        .map(|w| {
            let (head, seq) = shape(w);
            (head.to_string(), seq)
        })
        .collect();
    rows.extend(Noted::ALL.map(|n| (n.printed().to_string(), false)));
    // The real run's shapes (a reply's last line, a box's classification and command, its
    // approval, a question's answer), each head read to the end of what the grammar reads.
    rows.extend(
        [
            "EVENT idle seq=7 complete=1 rows=689",
            "EVENT question seq=7",
            "EVENT prompt seq=7",
            "EVENT turn seq=7 report=5 rows=12",
            "APPROVED seq=7",
            "CHOSE seq=7 rule=answer-recommended@v1",
        ]
        .map(|h| (h.to_string(), true)),
    );
    let tails = [
        format!("⏺ {PLANT} is the reply's last line"),
        format!("kind=bash classify=not-read-only:touch command=touch {PLANT}"),
        format!("touch {PLANT}"),
        format!("policy=recommended {PLANT} (Recommended)"),
        PLANT.to_string(),
    ];
    for (head, seq) in &rows {
        for tail in &tails {
            let line = format!("{head} {tail}");
            let other = format!("{head} {}", "x".repeat(tail.len()));
            let words = log_words(&line);
            assert_eq!(
                words,
                log_words(&other),
                "{line}: the tail's bytes reach it"
            );
            assert_eq!(
                words,
                format!("{head} text={}B", tail.len()),
                "{line}: its head and the tail's length"
            );
            assert!(!words.contains("QZP7WORD"), "{words}");
            assert_eq!(words.contains(" seq=7 "), *seq, "{words}");
        }
        assert_eq!(&log_words(head), head, "an empty tail has no text=");
    }
    let word_less = format!("PROBING seq=102 rule=usage-probe@v1 /usage {PLANT}");
    assert_eq!(
        log_words(&word_less),
        format!("other text={}B", word_less.len()),
        "a line of no word is `other` and its length"
    );
    assert_eq!(log_words(""), "other");
    assert_eq!(
        log_words(&format!("EVENT ⏺{PLANT} seq=7 x")),
        "EVENT ? seq=7 text=1B",
        "a head token that is no word is `?`"
    );
}

/// The JSON round-trips, escapes what it must, and prints `null` for what
/// the line did not say.
#[test]
fn a_record_round_trips_through_its_json() {
    let r = JournalRecord::of_line(
        1_789_407_079_123,
        Some("@s-1e91"),
        "EVENT idle seq=5 complete=0 rows=3 ⏺ \"quoted\" \\ tab\t",
        Some(8),
    );
    let json = r.to_json();
    assert!(
        json.starts_with(
            "{\"t\":1789407079123,\"sid\":\"s-1e91\",\"kind\":\"event\",\"phase\":\"idle\",\
         \"seq\":5,\"complete\":0,\"rows\":3,\"summary\":\"⏺ \\\"quoted\\\" \\\\ tab\\t\""
        ),
        "{json}"
    );
    assert!(json.ends_with(",\"turn\":8,\"report\":null}"), "{json}");
    assert_eq!(JournalRecord::from_json(&json), Ok(r));
    let t = JournalRecord::of_line(1, None, "TIMEOUT", None).to_json();
    assert_eq!(
        t,
        "{\"t\":1,\"sid\":null,\"kind\":\"timeout\",\"phase\":\"-\",\"seq\":null,\
         \"complete\":null,\"rows\":null,\"summary\":\"\",\"line\":\"TIMEOUT\",\"turn\":null,\
         \"report\":null}"
    );
    let r = JournalRecord::of_line(2, Some("s-1"), "EVENT turn seq=9 report=5 rows=3 x", None);
    let json = r.to_json();
    assert!(json.ends_with(",\"turn\":null,\"report\":5}"), "{json}");
    assert_eq!(JournalRecord::from_json(&json), Ok(r));
    let old = JournalRecord::from_json(
        "{\"t\":1,\"kind\":\"event\",\"phase\":\"idle\",\"line\":\"EVENT idle seq=1 x\"}",
    )
    .expect("an older record");
    assert_eq!((old.report, old.turn, old.mark), (None, None, None));
    let mut r = JournalRecord::of_line(3, Some("s-1"), "EVENT idle seq=4 ATERM-TASK-DONE", None);
    r.mark = Some(0xfeed_0000_0000_beef);
    let json = r.to_json();
    assert!(
        json.ends_with(",\"report\":null,\"mark\":\"feed00000000beef\"}"),
        "{json}"
    );
    assert_eq!(JournalRecord::from_json(&json), Ok(r));
    let mut r = JournalRecord::of_line(
        5,
        Some("s-1"),
        "DONE seq=4 rule=finished@v1 the worker reported ATERM-TASK-DONE",
        None,
    );
    assert!(!r.to_json().contains("ended"), "{}", r.to_json());
    r.ended = Some(1_792_458_186_177);
    let json = r.to_json();
    assert!(
        json.ends_with(",\"report\":null,\"ended\":1792458186177}"),
        "{json}"
    );
    assert_eq!(JournalRecord::from_json(&json), Ok(r.clone()));
    r.ended = Some(-42);
    assert_eq!(JournalRecord::from_json(&r.to_json()), Ok(r));
    assert_eq!(old.ended, None);
    assert!(JournalRecord::from_json("{\"kind\":\"event\"}").is_err());
    assert!(JournalRecord::from_json("{\"t\":1,\"kind\":\"event\",\"line\":").is_err());
}

/// A journal write that fails with `ENOSPC` or `EDQUOT` pokes the disk watch naming the
/// journal; any other failure pokes nothing.
#[test]
fn a_journal_write_out_of_space_pokes_the_disk_watch() {
    use crate::harness::disk::poke::{Cause, set_handler};
    use std::io::{Error, ErrorKind};
    use std::sync::{Arc, Mutex, PoisonError};
    static HEARD: Mutex<Vec<String>> = Mutex::new(Vec::new());
    set_handler(Arc::new(|cause| {
        if let Cause::Write(w) = cause {
            HEARD
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(w.clone());
        }
    }));
    let named = |tag: &str| {
        HEARD
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|w| w.contains(tag))
    };
    let denied = format!("journal-denied-{}", std::process::id());
    let mut j = Journal::named(Some(Path::new(&denied)), None);
    let mut warn = Vec::new();
    j.failed(
        &Error::from(ErrorKind::PermissionDenied),
        "cannot append to it",
        &mut warn,
    );
    assert!(!named(&denied), "no full disk, no poke");
    assert!(!warn.is_empty(), "said");
    for kind in [ErrorKind::StorageFull, ErrorKind::QuotaExceeded] {
        let full = format!("journal-full-{kind:?}-{}", std::process::id());
        let mut j = Journal::named(Some(Path::new(&full)), None);
        j.failed(&Error::from(kind), "cannot append to it", &mut Vec::new());
        assert!(named(&full), "{kind:?} pokes the watch");
    }
}
