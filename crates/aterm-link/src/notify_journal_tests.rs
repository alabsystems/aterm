// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::notify::{DIRS_FLUSHED, REFUSE};

/// The frozen line-format store's files, as a build before the journal wrote them.
const FROZEN: [(&str, &str); 4] = [
    (
        "journal",
        include_str!("../tests/fixtures/notify-line-format/journal"),
    ),
    (
        "rate",
        include_str!("../tests/fixtures/notify-line-format/rate"),
    ),
    (
        "suppressed",
        include_str!("../tests/fixtures/notify-line-format/suppressed"),
    ),
    (
        "standing",
        include_str!("../tests/fixtures/notify-line-format/standing"),
    ),
];

/// The stamps the frozen `rate` holds.
pub(crate) const FROZEN_STAMPS: [u64; 2] = [1_794_437_161_631, 1_794_437_161_645];

/// The count and mark the frozen `suppressed` holds.
pub(crate) const FROZEN_SUPPRESSED: (u64, u64) = (1, 1_794_437_161_643);

/// The verdicts the frozen `journal` holds, oldest first.
pub(crate) fn frozen_lines() -> Vec<String> {
    FROZEN[0].1.lines().map(str::to_string).collect()
}

/// Lay the frozen store's files in `root`, a notify store's root.
pub(crate) fn lay_frozen_store(root: &Path) {
    std::fs::create_dir_all(root.join("cur")).expect("the store");
    for (name, text) in FROZEN {
        std::fs::write(root.join(name), text).expect("a frozen file");
    }
}

fn store(tag: &str) -> (aterm_tempfile::TempDir, Store) {
    let dir = crate::test_scratch_dir(tag);
    let root = dir.path().join("notify");
    std::fs::create_dir_all(root.join("cur")).expect("the store");
    (dir, Store { root })
}

fn verdict(line: &str) -> Step {
    Step {
        verdict: Some(line.to_string()),
        ..Step::default()
    }
}

#[test]
fn a_step_reads_back_as_written_and_one_no_body_carries_is_refused() {
    let steps = [
        verdict("1 fired on=attention rc=0 dup=0"),
        Step {
            verdict: Some("2 firing on=need:approval t=9".into()),
            spent: vec![9],
            suppressed: None,
            stand: Vec::new(),
            cursor: Vec::new(),
        },
        Step {
            verdict: Some("3 dropped on=halt reason=rate limit=1/60000ms suppressed=4 t=12".into()),
            spent: Vec::new(),
            suppressed: Some((4, 12)),
            stand: Vec::new(),
            cursor: Vec::new(),
        },
        Step {
            verdict: None,
            spent: vec![7, 8],
            suppressed: Some((0, 3)),
            stand: Vec::new(),
            cursor: Vec::new(),
        },
    ];
    for step in &steps {
        assert_eq!(
            Step::of_record(step.body().as_bytes(), b""),
            Some(step.clone())
        );
    }
    let (bytes, next) = encode_from(0, &steps).expect("frames");
    let read = read(&bytes);
    assert_eq!((read.end, read.next_step, next), (End::Clean, 4, 4));
    assert_eq!(
        (
            Vec::from(read.index.lines.clone()),
            read.index.stamps.clone(),
            read.index.suppressed
        ),
        (
            vec![
                "1 fired on=attention rc=0 dup=0".to_string(),
                "2 firing on=need:approval t=9".into(),
                "3 dropped on=halt reason=rate limit=1/60000ms suppressed=4 t=12".into(),
            ],
            vec![7, 8, 9],
            (0, 3)
        )
    );
    for refused in [
        verdict("1 fired\nsuppressed 9 9"),
        verdict(""),
        Step::default(),
    ] {
        assert!(
            encode_from(0, std::slice::from_ref(&refused)).is_err(),
            "{refused:?} is no step a read takes back"
        );
    }
}

#[test]
fn the_newest_verdict_of_an_offset_replaces_its_older_one_and_the_oldest_go_past_the_keep() {
    let mut held = Verdicts::default();
    held.apply(verdict("5 firing on=attention t=1"));
    held.apply(verdict("6 dropped on=attention t=2"));
    held.apply(verdict("5 fired on=attention rc=0 dup=0"));
    assert_eq!(
        Vec::from(held.lines.clone()),
        [
            "6 dropped on=attention t=2",
            "5 fired on=attention rc=0 dup=0"
        ]
    );
    for off in 100..100 + JOURNAL_KEEP as u64 {
        held.apply(verdict(&format!("{off} fired on=attention rc=0 dup=0")));
    }
    assert_eq!(held.lines.len(), JOURNAL_KEEP);
    assert_eq!(
        held.lines.front().map(String::as_str),
        Some("100 fired on=attention rc=0 dup=0")
    );
}

/// A cut append, and a broken last step with nothing whole after it, read as the steps before
/// them; the open rewrites the file to hold exactly those, and appends go on after them.
#[test]
fn a_torn_last_step_recovers() {
    let steps: Vec<Step> = (1..=3)
        .map(|off| verdict(&format!("{off} fired on=attention rc=0 dup=0")))
        .collect();
    let (whole, _) = encode_from(0, &steps).expect("frames");
    let (fourth, _) =
        encode_from(3, &[verdict("4 fired on=attention rc=0 dup=0")]).expect("a frame");
    for (tear, bytes) in [
        (
            "cut",
            [whole.as_slice(), &fourth[..fourth.len() / 2]].concat(),
        ),
        ("broken", {
            let mut b = [whole.as_slice(), fourth.as_slice()].concat();
            let last = b.len() - 1;
            b[last] ^= 0x55;
            b
        }),
    ] {
        let (_dir, store) = store(&format!("torn-{tear}"));
        std::fs::write(store.root.join(FILE), &bytes).expect("a torn journal");
        assert!(
            matches!(read(&bytes).end, End::Cut | End::Torn),
            "PRECONDITION ({tear}): a torn tail"
        );
        let mut file = store.load().expect("the open");
        assert_eq!(file.held.lines.len(), 3, "{tear}: the whole steps");
        assert_eq!(
            std::fs::read(store.root.join(FILE)).expect("the journal"),
            whole,
            "{tear}: rewritten to hold them alone"
        );
        file.write(
            &[verdict("4 fired on=attention rc=0 dup=0")],
            Flush::Ordered,
            None,
        )
        .expect("an append");
        assert_eq!(store.journal().len(), 4, "{tear}: and it goes on");
    }
}

/// Damage before an intact step is a fault, never a torn tail: the bytes are kept as
/// [`FAULT`] for whoever repairs it, and every whole step before and after the damage is read.
#[test]
fn damage_before_an_intact_step_reads_as_a_fault() {
    let steps: Vec<Step> = (1..=3)
        .map(|off| verdict(&format!("{off} fired on=attention rc=0 dup=0")))
        .collect();
    let (mut bytes, _) = encode_from(0, &steps).expect("frames");
    let (first, _) = encode_from(0, &steps[..1]).expect("a frame");
    bytes[first.len() + 30] ^= 0x55;
    assert!(
        matches!(read(&bytes).end, End::Fault { .. }),
        "PRECONDITION: damage before an intact step reads as a fault: {:?}",
        read(&bytes).end
    );
    let (_dir, store) = store("fault");
    std::fs::write(store.root.join(FILE), &bytes).expect("a damaged journal");
    let file = store.load().expect("the open");
    assert_eq!(
        Vec::from(file.held.lines.clone()),
        [
            "1 fired on=attention rc=0 dup=0",
            "3 fired on=attention rc=0 dup=0"
        ],
        "every whole step, before and after the damage"
    );
    assert_eq!(
        std::fs::read(store.root.join(FAULT)).expect("kept"),
        bytes,
        "the damaged bytes are kept"
    );
    assert_eq!(
        read(&std::fs::read(store.root.join(FILE)).expect("the journal")).end,
        End::Clean
    );
    drop(file);
    let mut again = bytes.clone();
    again[first.len() + 31] ^= 0x55;
    std::fs::write(store.root.join(FILE), &again).expect("damaged again");
    let _ = store.load().expect("the open");
    assert_eq!(
        (
            std::fs::read(store.root.join(FAULT)).expect("the first kept"),
            std::fs::read(store.root.join(format!("{FAULT}.1"))).expect("the second kept")
        ),
        (bytes, again),
        "a second fault is kept beside the first, never over it"
    );
}

/// A FROZEN LINE-FORMAT STORE MIGRATES ONCE: its verdicts, stamps, count and standing values
/// read as the line files said, into one journal, and only then are the files set aside; a
/// migration whose journal write is refused sets nothing aside, and a later open migrates it
/// then. Once migrated, nothing reads the verdicts' line files again.
#[test]
fn a_frozen_line_format_journal_migrates() {
    let (_dir, store) = store("frozen");
    lay_frozen_store(&store.root);
    REFUSE.with(|r| *r.borrow_mut() = vec![(FILE, 0, libc::ENOSPC)]);
    assert!(store.load().is_err(), "the journal's write refused");
    for name in LINE_FILES.into_iter().chain([STANDING]) {
        assert!(store.root.join(name).is_file(), "{name} is not set aside");
    }
    assert!(!store.root.join(FILE).exists());
    let file = store.load().expect("the migration");
    assert_eq!(
        (
            Vec::from(file.held.lines.clone()),
            file.held.stamps.clone(),
            file.held.suppressed
        ),
        (frozen_lines(), FROZEN_STAMPS.to_vec(), FROZEN_SUPPRESSED),
        "what the line files said"
    );
    for name in LINE_FILES {
        assert!(!store.root.join(name).exists(), "{name} set aside");
        assert_eq!(
            std::fs::read_to_string(store.root.join(format!("{name}{SET_ASIDE}")))
                .expect("set aside"),
            FROZEN
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, t)| *t)
                .unwrap_or_default(),
            "{name} kept as it was"
        );
    }
    assert_eq!(
        file.held.standing,
        parse_standing(FROZEN[3].1),
        "the standing values migrated"
    );
    assert_eq!(file.held.standing.len(), 2, "PRECONDITION: two values");
    assert!(!store.root.join(STANDING).exists(), "standing set aside");
    assert_eq!(
        std::fs::read_to_string(store.root.join(format!("{STANDING}{SET_ASIDE}")))
            .expect("set aside"),
        FROZEN[3].1,
        "standing kept as it was"
    );
    assert_eq!(
        store.standing(),
        file.held.standing,
        "the journal holds them, read back"
    );
    std::fs::write(
        store.root.join("journal"),
        "9 fired on=attention rc=0 dup=0\n",
    )
    .expect("a stray line file");
    let again = store.load().expect("the open");
    assert_eq!(
        Vec::from(again.held.lines.clone()),
        frozen_lines(),
        "a line file after the migration is never read"
    );
    assert!(!store.root.join("journal").exists(), "and is set aside");
    assert_eq!(
        (
            std::fs::read_to_string(store.root.join(format!("journal{SET_ASIDE}")))
                .expect("the migrated one"),
            std::fs::read_to_string(store.root.join(format!("journal{SET_ASIDE}.1")))
                .expect("the stray one")
        ),
        (
            FROZEN[0].1.to_string(),
            "9 fired on=attention rc=0 dup=0\n".to_string()
        ),
        "beside the migrated one, never over it"
    );
}

/// The stamps read back in order of their value, whatever order they were spent in (an older
/// one `Notifier::restore` spends after a newer), so keeping the newest `limit` keeps the
/// newest by value, never one a later spend left behind.
#[test]
fn stamps_read_in_order_and_the_newest_by_value_are_kept() {
    let mut held = Verdicts::default();
    for spent in [vec![10, 20], vec![15], vec![20]] {
        held.apply(Step {
            spent,
            ..Step::default()
        });
    }
    assert_eq!(held.stamps, [10, 15, 20], "in order, each once");
    held.keep_stamps(1);
    assert_eq!(held.stamps, [20], "the newest by value");
}

/// A REWRITE CARRIES THE STAMPS THE NOTIFIER HOLDS: those it let go (out of the window, past
/// its limit) are not written again at every rewrite, and none it holds is lost.
#[test]
fn a_rewrite_carries_the_stamps_the_notifier_holds() {
    let (_dir, store) = store("rewrite-stamps");
    let mut file = store.load().expect("the open");
    file.write(
        &[Step {
            verdict: Some("1 firing on=attention t=1".into()),
            spent: vec![1, 2, 3],
            suppressed: None,
            stand: Vec::new(),
            cursor: Vec::new(),
        }],
        Flush::Ordered,
        Some(&[1, 2, 3]),
    )
    .expect("an append");
    file.bound = Bound { steps: 0, bytes: 0 };
    file.write(
        &[Step {
            verdict: Some("9 firing on=attention t=30".into()),
            spent: vec![30],
            suppressed: None,
            stand: Vec::new(),
            cursor: Vec::new(),
        }],
        Flush::Ordered,
        Some(&[30, 3]),
    )
    .expect("a rewrite");
    assert_eq!(
        (file.held.stamps.clone(), store.stamps()),
        (vec![3, 30], vec![3, 30]),
        "the rewrite holds and writes the notifier's stamps, in order"
    );
}

/// A REOPENED JOURNAL'S BOUND IS WHAT IT HOLDS WOULD REWRITE TO, never twice the file's own
/// steps: runs that each append short of the bound and restart keep the file within one bound
/// of what it holds, where a bound carried from the file's length doubles at each restart and
/// the file grows without end (the NEGATIVE CONTROL, [`OPEN_BOUND_FROM_FILE`]).
#[test]
fn a_reopened_journal_is_bounded_by_what_it_holds_not_by_its_length() {
    const RUN: u64 = COMPACT_FLOOR - 48;
    for mutant in [false, true] {
        let (_dir, store) = store(&format!("reopen-bound-{mutant}"));
        OPEN_BOUND_FROM_FILE.with(|m| m.set(mutant));
        let mut most = 0;
        for run in 0..4u64 {
            let mut file = store.load().expect("the open");
            if run > 0 && !mutant {
                assert_eq!(
                    file.bound.steps, COMPACT_FLOOR,
                    "run {run}: the bound of ten verdicts is the floor"
                );
            }
            for batch in 0..RUN / 100 {
                let steps: Vec<Step> = (0..100)
                    .map(|k| {
                        verdict(&format!(
                            "{} fired on=attention rc=0 dup=0 run={run} at={batch}.{k}",
                            k % 10
                        ))
                    })
                    .collect();
                file.write(&steps, Flush::Ordered, None).expect("appends");
            }
            most = most.max(file.steps);
            assert_eq!(file.held.lines.len(), 10, "PRECONDITION: ten offsets");
        }
        OPEN_BOUND_FROM_FILE.with(|m| m.set(false));
        let bytes = std::fs::read(store.root.join(FILE)).expect("the journal");
        if mutant {
            assert!(
                most > 3 * COMPACT_FLOOR,
                "NEGATIVE CONTROL: a bound carried from the file's length lets it grow past \
                 three floors over four runs: {most} steps"
            );
        } else {
            assert!(
                most <= COMPACT_FLOOR + 100,
                "four runs kept the file within one bound of what it holds: {most} steps"
            );
            assert_eq!(read(&bytes).end, End::Clean);
        }
    }
}

/// A LINE FILE THAT DOES NOT READ NEVER STOPS THE OPEN: it is migrated as empty, as a build
/// before the journal read it, and set aside with its bytes kept; the rest migrate.
#[test]
fn a_line_file_that_does_not_read_migrates_as_empty_and_is_kept() {
    let (_dir, store) = store("unreadable-line-file");
    lay_frozen_store(&store.root);
    std::fs::write(store.root.join("rate"), b"\xff\xfe not text").expect("a broken rate");
    let file = store.load().expect("the open migrates");
    assert_eq!(
        (
            Vec::from(file.held.lines.clone()),
            file.held.stamps.clone(),
            file.held.suppressed
        ),
        (frozen_lines(), Vec::new(), FROZEN_SUPPRESSED),
        "the rest migrate"
    );
    assert_eq!(
        std::fs::read(store.root.join(format!("rate{SET_ASIDE}"))).expect("kept"),
        b"\xff\xfe not text",
        "its bytes kept"
    );
}

/// What a carry reads of a store it must not change: the journal's whole steps, or a
/// line-format store's files, written nowhere; a fault is refused, and left as it was.
#[test]
fn a_read_of_verdicts_writes_nothing_and_refuses_a_fault() {
    let (_dir, store) = store("read-verdicts");
    lay_frozen_store(&store.root);
    assert_eq!(
        Vec::from(store.read_verdicts().expect("the line files").lines),
        frozen_lines()
    );
    for name in LINE_FILES {
        assert!(store.root.join(name).is_file(), "{name} untouched");
    }
    assert!(!store.root.join(FILE).exists(), "no journal written");
    let steps: Vec<Step> = (1..=3)
        .map(|off| verdict(&format!("{off} fired on=attention rc=0 dup=0")))
        .collect();
    let (mut bytes, _) = encode_from(0, &steps).expect("frames");
    let (first, _) = encode_from(0, &steps[..1]).expect("a frame");
    bytes[first.len() + 30] ^= 0x55;
    std::fs::write(store.root.join(FILE), &bytes).expect("a damaged journal");
    assert!(store.read_verdicts().is_err(), "a fault is refused");
    assert_eq!(
        std::fs::read(store.root.join(FILE)).expect("the journal"),
        bytes,
        "and left as it was"
    );
    assert!(!store.root.join(FAULT).exists(), "with nothing written");
}

/// A CHANGE OF THE STANDING VALUES IS A STEP: `stand <subject> <value>` sets one and makes it
/// the newest, `stand <subject>` clears it, applied in order; a rewrite writes a value a step,
/// the oldest set first, and reads back as the steps did. Each word is percent-encoded, so a
/// subject or value with a space, a control byte, a `%` or a byte past ASCII is a step too, its
/// body ASCII text; only an empty one is no step a read takes back.
#[test]
fn a_standing_change_is_a_step_and_the_newest_per_subject_is_read() {
    let set = |s: &str, v: &str| (s.to_string(), Some(v.to_string()));
    let steps = [
        Step {
            stand: vec![set("/a", "one"), set("/b", "two")],
            ..Step::default()
        },
        Step {
            verdict: Some("3 dropped on=need:failure t=12".into()),
            suppressed: Some((1, 12)),
            ..Step::default()
        },
        Step {
            stand: vec![
                set("/a", "three"),
                ("/b".to_string(), None),
                set("/c", "!x"),
            ],
            ..Step::default()
        },
    ];
    for step in &steps {
        assert_eq!(
            Step::of_record(step.body().as_bytes(), b""),
            Some(step.clone())
        );
    }
    let (bytes, _) = encode_from(0, &steps).expect("frames");
    let held = read(&bytes).index;
    let want = vec![
        ("/a".to_string(), "three".to_string()),
        ("/c".to_string(), "!x".to_string()),
    ];
    assert_eq!(
        held.standing, want,
        "the newest per subject, the oldest set first"
    );
    let (whole, n) = held.encode().expect("a rewrite");
    let back = read(&whole).index;
    assert_eq!((back.standing.clone(), back.lines.len()), (want, 1));
    assert_eq!(n, 1 + 1 + 2, "a verdict, the count, a value a step");
    for odd in [
        set("/a b", "one"),
        set("/a", "one two"),
        set("/a", "t\u{1}ab\t%41"),
        set("/\u{e5}", "\u{e5}\u{a5}"),
        ("/a\u{7f}".to_string(), None),
    ] {
        let step = Step {
            stand: vec![odd],
            ..Step::default()
        };
        let body = step.body();
        assert!(
            body.bytes()
                .all(|b| b == b' ' || b == b'\n' || b.is_ascii_graphic()),
            "{body:?} is ASCII text"
        );
        assert_eq!(
            Step::of_record(body.as_bytes(), b""),
            Some(step.clone()),
            "{step:?} reads back"
        );
        let (bytes, _) = encode_from(0, std::slice::from_ref(&step)).expect("frames");
        assert_eq!(
            read(&bytes).index.standing.len(),
            usize::from(step.stand[0].1.is_some())
        );
    }
    for refused in [
        Step {
            stand: vec![set("", "one")],
            ..Step::default()
        },
        Step {
            stand: vec![set("/a", "")],
            ..Step::default()
        },
        Step {
            stand: vec![("".to_string(), None)],
            ..Step::default()
        },
    ] {
        assert!(
            encode_from(0, std::slice::from_ref(&refused)).is_err(),
            "{refused:?} is no step a read takes back"
        );
    }
}

/// A STANDING LINE FILE BESIDE A JOURNAL MIGRATES, SET ASIDE ONLY AFTER ITS WRITE: a store a
/// build with the journal but not the standing values in it wrote (`journal.steps` and a
/// line-format `standing`) has the standing values written into the journal, its verdicts
/// kept; a refused write leaves the line file in place for the next open; a line file written
/// again later (a build before the change ran since) is the newest, read, and kept beside the
/// first set aside, never over it.
#[test]
fn a_standing_line_file_beside_a_journal_migrates_and_is_set_aside_only_after_its_write() {
    let (_dir, store) = store("standing-beside");
    store
        .set_journal(
            &["1 fired on=attention rc=0 dup=0".to_string()],
            Flush::Ordered,
        )
        .expect("a journal");
    std::fs::write(store.root.join(STANDING), FROZEN[3].1).expect("a standing file");
    REFUSE.with(|r| *r.borrow_mut() = vec![(FILE, 0, libc::ENOSPC)]);
    assert!(store.load().is_err(), "the journal's write refused");
    assert!(store.root.join(STANDING).is_file(), "not set aside");
    let file = store.load().expect("the migration");
    assert_eq!(
        (
            file.held.standing.clone(),
            Vec::from(file.held.lines.clone())
        ),
        (
            parse_standing(FROZEN[3].1),
            vec!["1 fired on=attention rc=0 dup=0".to_string()]
        ),
        "the standing values migrated beside the verdicts"
    );
    assert!(!store.root.join(STANDING).exists(), "set aside");
    drop(file);
    std::fs::write(store.root.join(STANDING), "/x newer\n").expect("written again");
    let again = store.load().expect("the open");
    assert_eq!(
        again.held.standing,
        [("/x".to_string(), "newer".to_string())],
        "the newest standing file is read"
    );
    assert_eq!(
        (
            std::fs::read_to_string(store.root.join(format!("{STANDING}{SET_ASIDE}")))
                .expect("the first"),
            std::fs::read_to_string(store.root.join(format!("{STANDING}{SET_ASIDE}.1")))
                .expect("the second")
        ),
        (FROZEN[3].1.to_string(), "/x newer\n".to_string()),
        "beside the first, never over it"
    );
    drop(again);
    assert_eq!(
        store.load().expect("a reopen").held.standing,
        [("/x".to_string(), "newer".to_string())],
        "and the journal holds it"
    );
}

/// A STANDING LINE FILE WHOSE RENAME ASIDE IS REFUSED FAILS THE OPEN: its values are in the
/// journal, but a load that went on would run with the file there, and the next load would lay
/// it over every newer step (a need cleared since read as standing). So the open fails, the
/// file stays, and the next open, its rename let through, migrates it again and sets it aside;
/// a newer step written after that is what a later open reads.
#[test]
fn a_standing_line_file_whose_rename_aside_is_refused_fails_the_open() {
    let (_dir, store) = store("standing-rename-refused");
    std::fs::write(store.root.join(STANDING), "/x old\n").expect("a standing file");
    REFUSE.with(|r| *r.borrow_mut() = vec![("standing.migrated", 0, libc::EPERM)]);
    let refused = store
        .load()
        .map(|_| ())
        .expect_err("the rename refused fails the open");
    assert_eq!(refused.raw_os_error(), Some(libc::EPERM));
    assert!(store.root.join(STANDING).is_file(), "the file stays");
    let mut file = store.load().expect("the migration, its rename let through");
    assert!(!store.root.join(STANDING).exists(), "set aside");
    file.write(
        &[Step {
            stand: vec![("/x".to_string(), None)],
            ..Step::default()
        }],
        Flush::Ordered,
        None,
    )
    .expect("a newer step clears it");
    drop(file);
    assert_eq!(
        store.load().expect("a reopen").held.standing,
        [],
        "the newer step is read, no stale file over it"
    );
}

/// A cursor written with the change it orders, as [`super::Notifier::put_standing`] writes it:
/// subject `/x` cleared, `need`'s cursor at the record that cleared it.
fn stand_and_cursor(at: u64) -> Step {
    Step {
        stand: vec![("/x".to_string(), None)],
        cursor: vec![("need".to_string(), at)],
        ..Step::default()
    }
}

/// A CURSOR IS A STEP, AND THE NEWEST PER KEY IS READ: `cursor <key> <off>` reads back as
/// written beside the standing change it orders, a later step of a key replaces its earlier
/// one, other keys stand, a reopen reads each key's newest, and a rewrite at the bound writes
/// each key's newest once, so the bound counts the cursors as what they rewrite to (two keys
/// written 2,000 times rewrite to one step).
#[test]
fn a_cursor_is_a_step_and_the_newest_per_key_is_read() {
    let step = stand_and_cursor(7);
    assert_eq!(
        Step::of_record(step.body().as_bytes(), b""),
        Some(step.clone()),
        "a stand and its cursor read back as one step"
    );
    let (_dir, store) = store("cursor-steps");
    let mut file = store.load().expect("the open");
    file.write(
        &[Step {
            stand: vec![("/x".to_string(), Some("help".to_string()))],
            ..Step::default()
        }],
        Flush::Ordered,
        None,
    )
    .expect("a value");
    file.write(&[step], Flush::Ordered, None).expect("a clear");
    for off in 8..2_008 {
        for key in ["need", "ask-to.h-a"] {
            file.write(
                &[Step {
                    cursor: vec![(key.to_string(), off)],
                    ..Step::default()
                }],
                Flush::Ordered,
                None,
            )
            .expect("a checkpoint");
        }
    }
    let want: BTreeMap<String, u64> = [
        ("ask-to.h-a".to_string(), 2_007),
        ("need".to_string(), 2_007),
    ]
    .into_iter()
    .collect();
    assert_eq!(file.held.cursors, want, "the newest per key");
    assert!(file.held.standing.is_empty(), "the clear stands");
    assert!(
        file.steps <= file.bound.steps,
        "4,000 checkpoints stay within the bound ({} steps, bound {:?})",
        file.steps,
        file.bound
    );
    drop(file);
    let back = store.load().expect("a reopen");
    assert_eq!(back.held.cursors, want, "a reopen reads each key's newest");
    let (_, steps) = back.held.encode().expect("a rewrite");
    assert_eq!(steps, 1, "a rewrite writes the cursors as one step");
    assert_eq!(store.read_cursor("need"), Some(2_007));
}

/// A CURSOR AND THE CHANGE IT ORDERS REACH THE MEDIA TOGETHER (notify-cursor-per-log, by
/// construction): a journal cut at every byte of the step that holds a standing value's clear
/// and its selector's cursor at the clearing record opens with both or neither, never one
/// without the other. The NEGATIVE CONTROL is the order a build before the cursor steps wrote:
/// the cursor's file (`cur/<key>`) on the media first and the clear's step after it, cut at
/// every byte, opens with the cursor past the record and the value it cleared still standing.
#[test]
fn a_cursor_and_the_change_it_orders_reach_the_media_together() {
    let clear = Step {
        stand: vec![("/x".to_string(), None)],
        ..Step::default()
    };
    for mutant in [false, true] {
        let (_dir, at) = store(&format!("cursor-crash-{mutant}"));
        let mut file = at.load().expect("the open");
        file.write(
            &[Step {
                stand: vec![("/x".to_string(), Some("help".to_string()))],
                cursor: vec![("need".to_string(), 3)],
                ..Step::default()
            }],
            Flush::Ordered,
            None,
        )
        .expect("a value");
        drop(file);
        let before = std::fs::read(at.root.join(FILE)).expect("the journal");
        let step = if mutant {
            clear.clone()
        } else {
            stand_and_cursor(7)
        };
        let (frame, _) = encode_from(1, &[step]).expect("a frame");
        let (mut apart, mut seen) = (0, std::collections::BTreeSet::new());
        for cut in 0..=frame.len() {
            std::fs::write(
                at.root.join(FILE),
                [before.as_slice(), &frame[..cut]].concat(),
            )
            .expect("a cut journal");
            if mutant {
                std::fs::create_dir_all(at.root.join(CURSORS)).expect("cur/");
                std::fs::write(at.root.join(CURSORS).join("need"), "7\n")
                    .expect("the cursor's file, on the media first");
            }
            let held = at.load().expect("the open").held;
            let cleared = held.standing.is_empty();
            let moved = held.cursors.get("need") == Some(&7);
            apart += usize::from(cleared != moved);
            seen.insert(cleared);
        }
        assert_eq!(
            seen.len(),
            2,
            "PRECONDITION ({mutant}): the cuts reach both ends"
        );
        if mutant {
            assert!(
                apart > 0,
                "NEGATIVE CONTROL: the cursor's file ahead of the clear's step leaves them apart"
            );
        } else {
            assert_eq!(
                apart, 0,
                "a cut at any byte of the step leaves the clear and its cursor both or neither"
            );
        }
    }
}

/// A CURSOR'S FILE MIGRATES AT THE OPEN AND IS SET ASIDE ONLY AFTER ITS WRITE: a store an
/// older build left (`cur/<key>` a selector, beside a journal that holds an older cursor of one
/// of them) opens with each file's offset as its key's cursor (it is the newest: only that
/// build writes one), the journal's other cursors standing; one that does not read migrates as
/// none, the journal's cursor of its key standing, and is kept; a temporary and a file set
/// aside before are no cursor. The journal is written first and the files set aside only
/// after (a refused write sets none aside), each beside an earlier one, never over it. A refused
/// rename aside fails the open, the file staying for the next, so no open lays an old cursor
/// over a newer step.
#[test]
fn a_cursor_file_migrates_at_the_open_and_is_set_aside_only_after_its_write() {
    let (_dir, store) = store("cursor-migrate");
    let mut file = store.load().expect("the open");
    file.write(
        &[Step {
            cursor: vec![("halt".to_string(), 40), ("need".to_string(), 2)],
            ..Step::default()
        }],
        Flush::Ordered,
        None,
    )
    .expect("the journal's cursors");
    drop(file);
    let cur = store.root.join(CURSORS);
    std::fs::write(cur.join("need"), "12\n").expect("an old build's cursor");
    std::fs::write(cur.join("ask-to.h-a"), "5\n").expect("another");
    std::fs::write(cur.join("halt"), "not an offset\n").expect("one that does not read");
    std::fs::write(cur.join(".need.1234.tmp"), "99\n").expect("a temporary");
    std::fs::write(cur.join("attention.migrated"), "77\n").expect("set aside before");
    REFUSE.with(|r| *r.borrow_mut() = vec![(FILE, 0, libc::ENOSPC)]);
    assert!(store.load().is_err(), "the journal's write refused");
    assert!(cur.join("need").is_file(), "not set aside");
    let want: BTreeMap<String, u64> = [
        ("ask-to.h-a".to_string(), 5),
        ("halt".to_string(), 40),
        ("need".to_string(), 12),
    ]
    .into_iter()
    .collect();
    let file = store.load().expect("the migration");
    assert_eq!(file.held.cursors, want, "each file's offset, newest");
    drop(file);
    let names = |dir: &Path| {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("cur/")
            .map(|e| {
                e.expect("an entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    };
    assert_eq!(
        names(&cur),
        [
            ".need.1234.tmp",
            "ask-to.h-a.migrated",
            "attention.migrated",
            "halt.migrated",
            "need.migrated"
        ],
        "each set aside, its bytes kept"
    );
    assert_eq!(
        std::fs::read_to_string(cur.join("halt.migrated")).expect("kept"),
        "not an offset\n"
    );
    std::fs::write(cur.join("need"), "30\n").expect("written again by an older build");
    REFUSE.with(|r| *r.borrow_mut() = vec![("cur/need.migrated", 0, libc::EPERM)]);
    let refused = store
        .load()
        .map(|_| ())
        .expect_err("the rename refused fails the open");
    assert_eq!(refused.raw_os_error(), Some(libc::EPERM));
    assert!(cur.join("need").is_file(), "the file stays");
    let mut file = store.load().expect("the migration, its rename let through");
    assert_eq!(file.held.cursors.get("need"), Some(&30));
    assert_eq!(
        std::fs::read_to_string(cur.join("need.migrated.1")).expect("the second"),
        "30\n",
        "beside the first, never over it"
    );
    file.write(
        &[Step {
            cursor: vec![("need".to_string(), 31)],
            ..Step::default()
        }],
        Flush::Ordered,
        None,
    )
    .expect("a newer step");
    drop(file);
    assert_eq!(
        store.load().expect("a reopen").held.cursors.get("need"),
        Some(&31),
        "the newer step is read, no stale file over it"
    );
}

/// A CURSOR'S FILE SET ASIDE HAS ITS OWN DIRECTORY FLUSHED, AND ONE A POWER LOSS REVIVES NEVER
/// SETS A NEWER STEP'S CURSOR BACK (notify-cursor-per-log): the migration's rename aside of
/// `cur/<key>` moves an entry of [`CURSORS`], so that directory is the one flushed ordered, not
/// only the store's. Where the rename is lost all the same (the file back as it was after the
/// journal took it and newer steps moved its cursor on), the next open, and a read outside the
/// notifier, take the journal's newer cursor: a cursor only moves forward. The NEGATIVE
/// CONTROLS, run by hand: flushing only the store's directory fails the first assertion, and a
/// file's offset that replaces the journal's cursor (`insert` in place of [`forward`]) reads 12.
#[test]
fn a_cursor_file_set_aside_is_flushed_in_its_directory_and_a_revived_one_never_sets_it_back() {
    let (_dir, store) = store("cursor-revived");
    let cur = store.root.join(CURSORS);
    std::fs::write(cur.join("need"), "12\n").expect("an old build's cursor");
    DIRS_FLUSHED.with(|d| *d.borrow_mut() = Some(Vec::new()));
    let mut file = store.load().expect("the migration");
    let flushed = DIRS_FLUSHED
        .with(std::cell::RefCell::take)
        .unwrap_or_default();
    assert_eq!(file.held.cursors.get("need"), Some(&12), "migrated");
    assert!(
        flushed.contains(&cur),
        "the rename aside of cur/need flushes cur/: {flushed:?}"
    );
    file.write(
        &[Step {
            stand: vec![("/x".to_string(), Some("help".to_string()))],
            cursor: vec![("need".to_string(), 40)],
            ..Step::default()
        }],
        Flush::Ordered,
        None,
    )
    .expect("a standing change at 40");
    drop(file);
    std::fs::remove_file(cur.join("need.migrated")).expect("the rename lost");
    std::fs::write(cur.join("need"), "12\n").expect("the file as the power loss left it");
    assert_eq!(
        store.read_cursor("need"),
        Some(40),
        "a read takes the newer step"
    );
    let back = store.load().expect("the open after the power loss");
    assert_eq!(
        back.held.cursors.get("need"),
        Some(&40),
        "the revived file never sets the cursor back below the standing change at 40"
    );
    assert!(
        cur.join("need.migrated").is_file(),
        "and it is set aside again"
    );
}

/// A DAMAGED JOURNAL STILL READS ITS CURSORS OUTSIDE THE NOTIFIER: a reader that starts before
/// the notifier's open salvages a log's store ([`Store::read_cursor`]) reads the cursor of the
/// whole steps around the damage, as the open will, never none (which would start the
/// selector over at `--since`). A read for a carry ([`Store::read_verdicts`]) still refuses
/// the fault. The NEGATIVE CONTROL, run by hand: a read that refuses the fault reads none.
#[test]
fn a_damaged_journal_reads_its_cursor_around_the_damage() {
    let steps: Vec<Step> = [5, 7, 9]
        .into_iter()
        .map(|off| Step {
            cursor: vec![("need".to_string(), off)],
            ..Step::default()
        })
        .collect();
    let (mut bytes, _) = encode_from(0, &steps).expect("frames");
    let (first, _) = encode_from(0, &steps[..1]).expect("a frame");
    bytes[first.len() + 20] ^= 0x55;
    assert!(
        matches!(read(&bytes).end, End::Fault { .. }),
        "PRECONDITION: damage before an intact step: {:?}",
        read(&bytes).end
    );
    let (_dir, store) = store("cursor-fault");
    std::fs::write(store.root.join(FILE), &bytes).expect("a damaged journal");
    assert_eq!(
        store.read_cursor("need"),
        Some(9),
        "the newest whole step's cursor"
    );
    assert!(store.read_verdicts().is_err(), "a carry's read refuses it");
    assert_eq!(
        std::fs::read(store.root.join(FILE)).expect("the journal"),
        bytes,
        "read, never written"
    );
    assert_eq!(
        store.load().expect("the open").held.cursors.get("need"),
        Some(&9),
        "as the open salvages it"
    );
}
