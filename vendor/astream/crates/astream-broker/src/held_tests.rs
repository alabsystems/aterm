// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The held store's tests (`held.rs`), on real logs under hand clocks: R4's acceptance,
//! RAM bounded by the rules while one mail waits a week, a hold class past its bytes, an
//! age rule, the mail hold, a restart, a crash after every durable step (the held store's
//! own first, process crash and power loss, plaintext and sealed under anti-rollback), torn
//! and damaged held stores, the rewrite, the reads below a base, and every record's age
//! across restarts, a crash that loses stamped records, and a rewrite.
//!
//! Every clock is a hand clock handed to [`BrokerLog::retain_by_rules`]; nothing sleeps.
//!
//! NEGATIVE CONTROLS (each a planted break, scratch-reverted, that fails the test named): a
//! cut that is the minimum over every rule's floor
//! (`ram_follows_the_rules_while_one_unread_mail_waits_a_week`); a drop that raises no floor
//! (`a_hold_class_past_its_bytes_gives_up_its_oldest_and_tells_the_reader`); a cut that
//! carries nothing (`mail_is_held_until_its_reader_reads_it`,
//! `a_restart_reads_the_held_store_the_floors_and_the_rules_back`); frames of a compaction
//! that never landed applied at the open (`a_crash_after_any_durable_step_*`); a fetch
//! below the oldest that clamps (`r4_acceptance_*`); a class past its bytes cut back to
//! exactly them (`a_log_at_its_bounds_is_copied_a_bounded_share_of_what_it_takes`); and a
//! wake counted by the log's bytes, not each class's own
//! (`a_class_at_its_bytes_asks_for_nothing_while_another_class_takes_records`); and every
//! class reset to the base by a rules change (`a_rules_change_keeps_the_held_mail_readable`);
//! an open that restamps the retained suffix
//! (`an_age_rule_cuts_by_when_a_record_was_first_seen_across_restarts`); stamps read back
//! that are not cut to the head recovered, or cut in memory only
//! (`a_crash_that_loses_stamped_records_never_ages_the_ones_after_it`); and a rewrite's
//! snapshot without the stamps (`a_rewritten_store_keeps_the_ticks_stamps`).

use super::*;
use crate::compact::{intent_path, kept_path, staged_state_path, Meter, Seam, Step};
use crate::retention::record_bytes;
use crate::store::{BrokerLog, RetentionTick};
use crate::test_scratch::Scratch;
use crate::Rule;
use astream_wire::Offset;
use std::time::Duration;

const EV: &str = "/f/F/pub/n1/s1/ev";
const PRESENCE: &str = "/f/F/pub/n1/s1/presence";
const MAIL: &str = "/f/F/in/n1/from/a";
const MAIL2: &str = "/f/F/in/n2/from/a";
const INBOX: &str = "/f/F/cur/n1/node/inbox";
const BODY: &[u8] = b"0123456789abcdef";
const MINUTE: u64 = 60_000;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;
/// The hand clock's start (unix ms).
const T0: u64 = 1_800_000_000_000;

#[cfg(feature = "anti-rollback")]
const KEY: [u8; 32] = [0x5e; 32];

fn filter(f: &str) -> Filter {
    Filter::new(f).unwrap()
}

fn rec_of(subject: &str, body: &[u8]) -> BrokerRecord {
    BrokerRecord {
        seq: Offset(0),
        producer_id: 1,
        producer_seq: 1,
        subject: subject.to_string(),
        body: body.to_vec(),
        commit: None,
    }
}

/// The bytes one test record of `subject` takes ([`record_bytes`]).
fn rb(subject: &str) -> u64 {
    record_bytes(&rec_of(subject, BODY))
}

/// The bulk stream the prefix (by `ev_bytes`, `ev_age`), mail held until its node's inbox
/// group reads it (when `readers`) or for 7 days, within `mail_bytes`; presence by its last
/// value; the default rule by its last value.
fn rules(
    ev_bytes: Option<u64>,
    ev_age: Option<u64>,
    mail_bytes: Option<u64>,
    mail_age: u64,
    readers: bool,
) -> Retention {
    let mut ev = Rule::prefix("/f/F/pub/*/*/ev").unwrap();
    if let Some(b) = ev_bytes {
        ev = ev.max_bytes(b);
    }
    if let Some(a) = ev_age {
        ev = ev.max_age(Duration::from_millis(a));
    }
    let mut mail = Rule::hold("/f/F/in/>")
        .unwrap()
        .max_age(Duration::from_millis(mail_age));
    if let Some(b) = mail_bytes {
        mail = mail.max_bytes(b);
    }
    if readers {
        mail = mail.until_read("/f/{2}/cur/{4}/node/inbox").unwrap();
    }
    Retention::new(
        vec![
            ev,
            mail,
            Rule::hold("/f/F/pub/*/*/presence").unwrap().last_only(),
        ],
        Rule::default_hold(),
    )
    .unwrap()
}

/// One log in its own scratch directory, plaintext or sealed under anti-rollback.
struct Fx {
    root: Scratch,
    path: PathBuf,
    sealed: bool,
    /// The producer sequence the next publish takes.
    seq: u64,
}

impl Fx {
    fn new(tag: &str, sealed: bool) -> Fx {
        let root = Scratch::new(tag);
        let path = root.path().join("log");
        Fx {
            root,
            path,
            sealed,
            seq: 0,
        }
    }

    fn create(&self) -> BrokerLog {
        #[cfg(feature = "anti-rollback")]
        if self.sealed {
            return BrokerLog::open_encrypted_verified_init(&self.path, KEY).unwrap();
        }
        assert!(!self.sealed, "a sealed fixture needs anti-rollback");
        BrokerLog::open(&self.path).unwrap()
    }

    fn try_open(&self) -> io::Result<BrokerLog> {
        #[cfg(feature = "anti-rollback")]
        if self.sealed {
            return BrokerLog::open_encrypted_verified(&self.path, KEY);
        }
        BrokerLog::open(&self.path)
    }

    fn open(&self) -> BrokerLog {
        self.try_open().unwrap()
    }

    fn dir(&self) -> &Path {
        self.root.path()
    }

    /// Publish `subjects` as one batch; their offsets.
    fn batch(&mut self, log: &mut BrokerLog, subjects: &[&str]) -> Vec<u64> {
        let mut offs = Vec::new();
        for s in subjects {
            self.seq += 1;
            let (off, _) = log
                .stage_publish(1, self.seq, s.to_string(), BODY.to_vec())
                .unwrap();
            offs.push(off.0);
        }
        log.commit_batch().unwrap();
        offs
    }

    /// The compaction's and the held store's own files beside the log that are present.
    fn leftovers(&self) -> Vec<PathBuf> {
        [
            intent_path(&self.path),
            kept_path(&self.path),
            staged_state_path(&self.path),
            held_tmp_path(&self.path),
        ]
        .into_iter()
        .filter(|p| std::fs::symlink_metadata(p).is_ok())
        .collect()
    }
}

/// Every retained record's bytes and every held one's: what the log keeps in RAM.
fn ram(log: &BrokerLog) -> u64 {
    let retained: u64 = log
        .read_from(Offset(0))
        .iter()
        .map(|r| record_bytes(r))
        .sum();
    retained + log.stats().held_bytes
}

/// The offsets a read of `f` from the start returns (it starts at the filter's oldest).
fn read(log: &BrokerLog, f: &str) -> Vec<u64> {
    let (page, _) = log.fetch(0, &filter(f), 10_000, 100_000, log.head().0);
    page.iter().map(|r| r.seq.0).collect()
}

/// `Last` of `f` at the log's head, one bounded step, through an owned view as every
/// reader of `Last` now takes one (the head published visible first).
fn last_of(log: &mut BrokerLog, f: &Filter, max: usize, scan_max: usize) -> Vec<Arc<BrokerRecord>> {
    let head = log.head().0;
    drop(log.publish_last_visible(head).unwrap());
    let mut view = log.last_view_at(head).unwrap();
    let (page, _) = log
        .last_matching(f, "", max, scan_max, &mut view)
        .unwrap()
        .into_page()
        .expect("the fixture fits one bounded step");
    page
}

/// R4's ACCEPTANCE: retention of the bulk stream set to 100 records, 250 published beside a
/// presence record at offset 3 (251 records, offsets 0 to 250, in batches of ten): the
/// oldest the stream reads is 151 — the record itself, not rounded to its batch (150 to 159):
/// offsets are absolute and an open scans a log that starts inside a batch — a `Fetch` from
/// 5 is told `retained-from=151` (NC: one that clamps), and `Last` of the presence subject
/// still answers its record at 3, across a restart too.
#[test]
fn r4_acceptance_retaining_100_of_250_reads_oldest_151_and_last_still_answers() {
    let mut fx = Fx::new("held-r4", false);
    let mut log = fx.create();
    log.set_retention(rules(Some(100 * rb(EV)), None, None, 7 * DAY, true))
        .unwrap();
    let subjects: Vec<&str> = (0..=250u64)
        .map(|i| if i == 3 { PRESENCE } else { EV })
        .collect();
    for chunk in subjects.chunks(10) {
        fx.batch(&mut log, chunk);
    }
    assert_eq!(log.head(), Offset(251));
    let tick = log.retain_by_rules(T0).unwrap();
    assert_eq!((tick.base_before, tick.base, tick.dropped), (0, 151, 151));
    let ev = filter("/f/F/pub/*/*/ev");
    let presence = filter("/f/F/pub/*/*/presence");
    for pass in ["live", "after a restart"] {
        assert_eq!(log.oldest(&ev), Offset(151), "{pass}");
        let head = log.head().0;
        match log.fetch_checked(5, &ev, 10, 1000, head) {
            Err(o) => assert_eq!(o, Offset(151), "{pass}"),
            Ok((page, _)) => panic!("{pass}: a fetch from 5 answered {} records", page.len()),
        }
        let (page, _) = log.fetch_checked(151, &ev, 1, 1000, head).unwrap();
        assert_eq!(page[0].seq, Offset(151), "{pass}");
        let last = last_of(&mut log, &presence, 10, 100);
        assert_eq!(last.len(), 1, "{pass}");
        assert_eq!(
            last[0].seq,
            Offset(3),
            "{pass}: Last of the presence subject"
        );
        drop(log);
        log = fx.open();
    }
}

/// RAM FOLLOWS THE RULES: one unread mail at offset 1 waits a week while the bulk stream
/// floods past its bytes, a batch an hour, a tick an hour on the hand clock. After every
/// tick the retained and held bytes are within the sum of the rules' bytes (plus what the
/// log took since the planning: nothing, here), and the mail still reads at offset 1 until
/// its 7 days are up; then it is given up, told (`lost`, and a read below it is refused with
/// its floor), and gone. NC: a cut that is the minimum over every rule's floor keeps every
/// record since the mail, past the bound.
#[test]
fn ram_follows_the_rules_while_one_unread_mail_waits_a_week() {
    let mut fx = Fx::new("held-ram", false);
    let mut log = fx.create();
    let (ev_max, mail_max) = (40 * rb(EV), 4 * rb(MAIL));
    log.set_retention(rules(Some(ev_max), None, Some(mail_max), 7 * DAY, true))
        .unwrap();
    assert_eq!(fx.batch(&mut log, &[EV, MAIL]), vec![0, 1]);
    let batch = [EV; 10];
    let mut told = false;
    for hour in 0..(7 * 24 + 3) {
        fx.batch(&mut log, &batch);
        let now = T0 + hour * HOUR;
        let tick = log.retain_by_rules(now).unwrap();
        assert!(
            ram(&log) <= ev_max + mail_max,
            "hour {hour}: {} bytes in RAM, the rules allow {}",
            ram(&log),
            ev_max + mail_max
        );
        if now - T0 <= 7 * DAY {
            assert_eq!(
                read(&log, "/f/F/in/>"),
                vec![1],
                "hour {hour}: the mail reads"
            );
        } else {
            if !tick.lost.is_empty() {
                assert_eq!(tick.lost, vec![(MAIL.to_string(), 1)], "hour {hour}");
                told = true;
            }
            assert!(
                read(&log, "/f/F/in/>").is_empty(),
                "hour {hour}: the mail is gone"
            );
            let head = log.head().0;
            assert_eq!(
                log.fetch_checked(0, &filter("/f/F/in/n1/>"), 10, 100, head)
                    .map(|(p, n)| (p.len(), n)),
                Err(Offset(2)),
                "hour {hour}: a read below it is told"
            );
        }
    }
    assert!(told, "the mail's drop was told once");
    assert!(log.base().0 > 1, "the cut went past the mail");
}

/// A LOG AT ITS BOUNDS IS COPIED A BOUNDED SHARE OF WHAT IT TAKES: the bulk stream at its 20
/// records' bytes takes 200 more, one a batch, each batch ticked when the log is due one (as
/// the broker's writer asks). Every cut gives up a quarter of the class, so the bytes the
/// compactions copy stay within four times the bytes the log took, and the class within its
/// bytes after every tick. NC: a class cut back to exactly its bytes is due a cut, which
/// copies the whole kept log, for every batch: about twenty times what the log took.
#[test]
fn a_log_at_its_bounds_is_copied_a_bounded_share_of_what_it_takes() {
    let mut fx = Fx::new("held-steady", false);
    let mut log = fx.create();
    let max = 20 * rb(EV);
    log.set_retention(rules(Some(max), None, None, 7 * DAY, true))
        .unwrap();
    for _ in 0..20 {
        fx.batch(&mut log, &[EV]);
    }
    let meter = Arc::new(Meter::default());
    log.arm_compact_seam(Seam {
        meter: Some(Arc::clone(&meter)),
        ..Seam::default()
    });
    let (mut took, mut ticks) = (0u64, 0u64);
    for i in 0..200u64 {
        let before = log.durable_len();
        fx.batch(&mut log, &[EV]);
        took += log.durable_len() - before;
        if log.retention_due() {
            log.retain_by_rules(T0 + i).unwrap();
            ticks += 1;
        }
        assert!(ram(&log) <= max + rb(EV), "batch {i}: {} bytes", ram(&log));
    }
    let copied: u64 = meter.flushes.lock().unwrap().iter().sum();
    assert!(
        log.base().0 >= 200,
        "PRECONDITION: the stream was cut ({ticks} ticks)"
    );
    assert!(
        copied <= 4 * took,
        "{copied} bytes copied for {took} taken, in {ticks} ticks"
    );
}

/// A CLASS AT ITS BYTES ASKS FOR NOTHING WHILE ANOTHER CLASS TAKES RECORDS: the mail class at
/// exactly its three mails' bytes, the bulk stream far from its own, fifty stream batches
/// leave the log due no tick; the next mail does. NC: a wake counted by the log's bytes, the
/// tightest class's room against every record, is due a tick, which plans the whole log,
/// after the first stream batch and every one after it.
#[test]
fn a_class_at_its_bytes_asks_for_nothing_while_another_class_takes_records() {
    let mut fx = Fx::new("held-quiet", false);
    let mut log = fx.create();
    log.set_retention(rules(
        Some(1000 * rb(EV)),
        None,
        Some(3 * rb(MAIL)),
        7 * DAY,
        false,
    ))
    .unwrap();
    fx.batch(&mut log, &[MAIL, MAIL, MAIL]);
    assert!(log.retention_due(), "before the first planning, any record");
    assert_eq!(log.retain_by_rules(T0).unwrap().base, 0);
    assert!(!log.retention_due(), "planned");
    for i in 0..50 {
        fx.batch(&mut log, &[EV]);
        assert!(!log.retention_due(), "stream batch {i}");
    }
    fx.batch(&mut log, &[MAIL]);
    assert!(log.retention_due(), "the mail class is past its bytes");
}

/// A HOLD CLASS PAST ITS BYTES GIVES UP ITS OLDEST, AND TELLS: five mails below the cut, the
/// class's bytes three mails'; the two oldest are given up (logged, counted in `lost`), and a
/// read of the mail from the start is refused `retained-from` the third (NC: a drop that
/// raises no floor reads the three silently), which reads on, across a restart too.
#[test]
fn a_hold_class_past_its_bytes_gives_up_its_oldest_and_tells_the_reader() {
    let mut fx = Fx::new("held-bytes", false);
    let mut log = fx.create();
    log.set_retention(rules(
        Some(2 * rb(EV)),
        None,
        Some(3 * rb(MAIL)),
        7 * DAY,
        false,
    ))
    .unwrap();
    fx.batch(&mut log, &[MAIL, MAIL, MAIL, MAIL, MAIL, EV, EV, EV]);
    let tick = log.retain_by_rules(T0).unwrap();
    assert_eq!(tick.base, 6);
    assert_eq!(tick.carried, 3);
    assert_eq!(tick.lost, vec![(MAIL.to_string(), 2)]);
    let mail = filter("/f/F/in/>");
    for pass in ["live", "after a restart"] {
        let head = log.head().0;
        assert_eq!(
            log.fetch_checked(0, &mail, 10, 100, head)
                .map(|(p, _)| p.len()),
            Err(Offset(2)),
            "{pass}: told where the class's records start"
        );
        let (page, next) = log.fetch_checked(2, &mail, 10, 100, head).unwrap();
        assert_eq!(
            page.iter().map(|r| r.seq.0).collect::<Vec<_>>(),
            vec![2, 3, 4],
            "{pass}"
        );
        assert_eq!(
            next, 8,
            "{pass}: the scan went on through the retained suffix"
        );
        assert_eq!(log.stats().held_records, 3, "{pass}");
        drop(log);
        log = fx.open();
    }
}

/// AN AGE RULE under the hand clock: the bulk stream keeps an hour, mail a day. Nothing is
/// cut before its age; at an hour and a minute the records the first tick stamped go; at a
/// day and two minutes the rest go, and the mail the cut carried is given up by its age and
/// told.
#[test]
fn an_age_rule_cuts_by_the_hand_clock() {
    let mut fx = Fx::new("held-age", false);
    let mut log = fx.create();
    log.set_retention(rules(None, Some(HOUR), None, DAY, false))
        .unwrap();
    fx.batch(&mut log, &[EV, EV, MAIL]);
    assert_eq!(
        log.retain_by_rules(T0).unwrap().base,
        0,
        "nothing is an hour old"
    );
    fx.batch(&mut log, &[EV, EV]);
    assert_eq!(log.retain_by_rules(T0 + 30 * MINUTE).unwrap().base, 0);
    let tick = log.retain_by_rules(T0 + HOUR + MINUTE).unwrap();
    assert_eq!(
        tick.base, 2,
        "the first tick's ev records are past the hour"
    );
    assert_eq!(
        read(&log, "/f/F/in/>"),
        vec![2],
        "the mail is retained above the cut"
    );
    let tick = log.retain_by_rules(T0 + DAY + 2 * MINUTE).unwrap();
    assert_eq!(tick.base, 5, "every ev record is past the hour");
    assert_eq!(
        tick.lost,
        vec![(MAIL.to_string(), 1)],
        "the mail is a day old"
    );
    let head = log.head().0;
    assert_eq!(
        log.fetch_checked(0, &filter("/f/F/in/>"), 10, 100, head)
            .map(|(p, _)| p.len()),
        Err(Offset(3))
    );
}

/// THE MAIL HOLD: an in/ mail below the cut is carried while its node's inbox group has not
/// read it (it reads at its own offset, below the base), and released once the group's
/// commit passes it — not told as lost: its reader has it. NC: a cut that carries nothing
/// drops the unread mail.
#[test]
fn mail_is_held_until_its_reader_reads_it() {
    let mut fx = Fx::new("held-mail", false);
    let mut log = fx.create();
    log.set_retention(rules(Some(rb(EV)), None, None, 7 * DAY, true))
        .unwrap();
    fx.batch(&mut log, &[EV, MAIL, MAIL2, EV, EV]);
    let tick = log.retain_by_rules(T0).unwrap();
    assert_eq!((tick.base, tick.carried), (4, 2));
    assert_eq!(read(&log, "/f/F/in/>"), vec![1, 2], "the unread mail reads");
    assert_eq!(log.oldest(&filter("/f/F/in/>")), Offset(0));
    log.commit(INBOX.into(), 1).unwrap();
    let tick = log.retain_by_rules(T0 + MINUTE).unwrap();
    assert_eq!(tick.released, 1, "n1's reader read its mail");
    assert!(tick.lost.is_empty(), "a read mail is not lost");
    assert_eq!(read(&log, "/f/F/in/>"), vec![2], "n2's is still held");
    assert_eq!(log.oldest(&filter("/f/F/in/n1/>")), Offset(2));
    assert_eq!(log.oldest(&filter("/f/F/in/n2/>")), Offset(0));
}

/// A RESTART reads back the held store, its floors, its rules and the carried state: the
/// mail held below the base reads at its offset, the floor still refuses a read below it,
/// `Last` answers, and the same rules set again keep the store whole and tick on. NC: a cut
/// that carries nothing leaves no mail to read back.
#[test]
fn a_restart_reads_the_held_store_the_floors_and_the_rules_back() {
    let mut fx = Fx::new("held-restart", false);
    let mut log = fx.create();
    let r = rules(Some(rb(EV)), None, Some(3 * rb(MAIL)), 7 * DAY, false);
    log.set_retention(r.clone()).unwrap();
    fx.batch(&mut log, &[MAIL, PRESENCE, MAIL, MAIL, MAIL, EV, EV]);
    let tick = log.retain_by_rules(T0).unwrap();
    assert_eq!((tick.base, tick.carried, tick.lost.len()), (6, 3, 1));
    drop(log);
    let mut log = fx.open();
    assert_eq!(log.retention(), Some(&r), "the rules are read back");
    assert_eq!(log.base(), Offset(6));
    assert_eq!(read(&log, "/f/F/in/>"), vec![2, 3, 4]);
    assert_eq!(log.oldest(&filter("/f/F/in/>")), Offset(1));
    let last = last_of(&mut log, &filter("/f/F/pub/*/*/presence"), 10, 100);
    assert_eq!(last.iter().map(|r| r.seq.0).collect::<Vec<_>>(), vec![1]);
    assert_eq!(
        log.retain_by_rules(T0 + MINUTE).unwrap(),
        RetentionTick {
            base_before: 6,
            base: 6,
            ..RetentionTick::default()
        },
        "rules read back are not set: no tick runs on them"
    );
    log.set_retention(r).unwrap();
    assert_eq!(fx.batch(&mut log, &[MAIL, EV]), vec![7, 8]);
    let tick = log.retain_by_rules(T0 + 2 * MINUTE).unwrap();
    assert_eq!(tick.base, 7, "the mail at 7 is retained above the cut");
    assert_eq!(
        tick.lost,
        vec![(MAIL.to_string(), 1)],
        "the class keeps three"
    );
    assert_eq!(read(&log, "/f/F/in/>"), vec![3, 4, 7]);
    drop(log);
    let log = fx.open();
    assert_eq!(read(&log, "/f/F/in/>"), vec![3, 4, 7]);
    assert_eq!(log.oldest(&filter("/f/F/in/>")), Offset(3));
}

/// What a reader can see of the crash law's log: the records, the mail held and retained,
/// the floors, `Last`, the inbox group's resume point and the store's counts.
#[derive(Debug, PartialEq, Eq)]
struct Snap {
    base: u64,
    head: u64,
    retained: Vec<(u64, String)>,
    mail: Vec<u64>,
    oldest: [u64; 3],
    last: Vec<(String, u64)>,
    group: u64,
    held: (usize, u64),
}

fn snap(log: &mut BrokerLog) -> Snap {
    let head = log.head().0;
    let last = last_of(log, &filter("/>"), 1000, 100_000);
    let stats = log.stats();
    Snap {
        base: log.base().0,
        head,
        retained: log
            .read_from(Offset(0))
            .iter()
            .map(|r| (r.seq.0, r.subject.clone()))
            .collect(),
        mail: read(log, "/f/F/in/>"),
        oldest: [
            log.oldest(&filter("/f/F/in/n1/>")).0,
            log.oldest(&filter("/f/F/in/n2/>")).0,
            log.oldest(&filter("/f/F/pub/*/*/ev")).0,
        ],
        last: last.iter().map(|r| (r.subject.clone(), r.seq.0)).collect(),
        group: log.group_start(INBOX).0,
        held: (stats.held_records, stats.held_bytes),
    }
}

/// The crash law's rules: three ev records kept, mail until read or 7 days.
fn crash_rules() -> Retention {
    rules(Some(3 * rb(EV)), None, None, 7 * DAY, true)
}

/// The log every crash case starts from: a first tick carried two mails (n1's at 1, n2's
/// at 4); n1's reader then read its mail; and more was published, n2 mail at 13 among it.
/// Reopened, its rules set again: the next tick at `T0 + HOUR` releases n1's mail (a floor
/// rises) and carries the mail at 13, cutting at 15.
fn seed(fx: &mut Fx) -> BrokerLog {
    let mut log = fx.create();
    log.set_retention(crash_rules()).unwrap();
    fx.batch(&mut log, &[EV, MAIL, EV, PRESENCE, MAIL2]);
    fx.batch(&mut log, &[EV; 6]);
    let tick = log.retain_by_rules(T0).unwrap();
    assert_eq!((tick.base, tick.carried), (8, 2));
    log.commit(INBOX.into(), 1).unwrap();
    fx.batch(&mut log, &[EV, MAIL2, EV, EV, EV, EV]);
    drop(log);
    let mut log = fx.open();
    log.set_retention(crash_rules()).unwrap();
    log
}

/// The reference: the seeded log as a reopen reads it before the tick, and after an
/// uninterrupted one.
fn reference(sealed: bool) -> (Snap, Snap) {
    let mut fx = Fx::new("held-ref", sealed);
    let mut log = seed(&mut fx);
    let before = snap(&mut log);
    drop(log);
    assert_eq!(snap(&mut fx.open()), before, "a reopen reads the seed back");
    let mut log = fx.open();
    log.set_retention(crash_rules()).unwrap();
    let tick = log.retain_by_rules(T0 + HOUR).unwrap();
    assert_eq!(
        (tick.base, tick.carried, tick.released),
        (15, 1, 1),
        "the tick both carries and releases"
    );
    drop(log);
    let after = snap(&mut fx.open());
    assert_eq!(after.base, 15);
    assert_eq!(after.mail, vec![4, 13]);
    assert_eq!(after.oldest, [2, 0, 15]);
    assert!(fx.leftovers().is_empty(), "{:?}", fx.leftovers());
    (before, after)
}

const STEPS: [Step; 9] = [
    Step::Held,
    Step::Copy,
    Step::State,
    Step::Intent,
    Step::Rename,
    Step::StateInstalled,
    Step::Base,
    Step::Watermark,
    Step::IntentRemoved,
];

/// Whether the log on disk is the kept suffix once `step` is durable.
const fn switched(step: Step) -> bool {
    !matches!(step, Step::Held | Step::Copy | Step::State | Step::Intent)
}

/// THE CRASH LAW WITH A HELD STORE: after a crash following any durable step of a tick's
/// compaction — the held store's frames first — whether the process stopped dead or the
/// power went (every directory that can leave), the next open reads the log, its held
/// store and its floors either as they were or as the uninterrupted tick leaves them, never
/// anything else: no carried mail lost, no released one back, no frame of a compaction that
/// did not land applied. It leaves none of the compaction's files behind, a second open
/// reads the same, and the log ticks on.
fn held_crash_law(sealed: bool) {
    let (before, after) = reference(sealed);
    for step in STEPS {
        if step == Step::Watermark && !sealed {
            continue;
        }
        let mut fx = Fx::new("held-crash", sealed);
        let mut log = seed(&mut fx);
        log.arm_compact_seam(Seam {
            crash_after: Some(step),
            ..Seam::default()
        });
        #[cfg(unix)]
        crate::power_loss::arm(fx.dir());
        let err = log.retain_by_rules(T0 + HOUR).expect_err("the armed crash");
        #[cfg(unix)]
        let lost = crate::power_loss::disarm().unwrap().images();
        #[cfg(not(unix))]
        let lost: Vec<()> = Vec::new();
        assert!(
            err.to_string().contains("injected crash"),
            "{step:?}: {err}"
        );
        drop(log);
        #[cfg(unix)]
        let stood = crate::power_loss::capture(fx.dir());
        let want = if switched(step) { &after } else { &before };
        #[cfg(unix)]
        {
            let left = std::iter::once(("a process crash", stood))
                .chain(lost.into_iter().map(|image| ("a power loss", image)));
            for (how, image) in left {
                crate::power_loss::apply(fx.dir(), &image);
                reopens_whole(&fx, &format!("{step:?}, {how}"), want);
            }
        }
        #[cfg(not(unix))]
        {
            let _ = lost;
            reopens_whole(&fx, &format!("{step:?}"), want);
        }
    }
}

fn reopens_whole(fx: &Fx, at: &str, want: &Snap) {
    let mut log = fx
        .try_open()
        .unwrap_or_else(|e| panic!("{at}: the open after the crash refused: {e}"));
    assert_eq!(&snap(&mut log), want, "{at}");
    drop(log);
    assert!(
        fx.leftovers().is_empty(),
        "{at}: left behind {:?}",
        fx.leftovers()
    );
    let mut log = fx
        .try_open()
        .unwrap_or_else(|e| panic!("{at}: the second open refused: {e}"));
    assert_eq!(&snap(&mut log), want, "{at}: the second open");
    log.set_retention(crash_rules()).unwrap();
    let tick = log.retain_by_rules(T0 + HOUR).unwrap();
    assert_eq!(tick.base, 15, "{at}: ticks on to the same cut");
    drop(log);
    let log = fx.open();
    assert_eq!(
        read(&log, "/f/F/in/>"),
        vec![4, 13],
        "{at}: and reads it back"
    );
}

#[test]
fn a_crash_after_any_durable_step_of_a_ticks_compaction_reopens_whole() {
    held_crash_law(false);
}

/// The same law on a log sealed at rest under anti-rollback, reopened verified: the held
/// store is sealed under the log's key, frame by frame.
#[cfg(feature = "anti-rollback")]
#[test]
fn a_crash_after_any_durable_step_under_anti_rollback_reopens_whole() {
    held_crash_law(true);
}

/// A SEALED HELD STORE does not open under another key, and holds no record's bytes in the
/// clear.
#[cfg(feature = "anti-rollback")]
#[test]
fn a_sealed_held_store_is_sealed() {
    let mut fx = Fx::new("held-sealed", true);
    let log = seed(&mut fx);
    drop(log);
    let bytes = std::fs::read(held_path(&fx.path)).unwrap();
    assert!(
        !bytes.windows(BODY.len()).any(|w| w == BODY),
        "a held record's body in the clear"
    );
    let err = HeldStore::load(&fx.path, Some(&[0x11; 32]), 8, false)
        .err()
        .expect("another key");
    assert!(err.to_string().contains("authenticate"), "{err}");
}

/// AGES SURVIVE A RESTART: the bulk stream kept 7 days by age; ten records first seen by a
/// tick at T0, the log reopened and its rules set again (as `asb serve --retain-file` does at
/// every start), ten more first seen at T0 + 3 days, reopened again: a tick at T0 + 7 days and
/// a minute cuts the first ten, by when they were first seen, and after one more restart a
/// tick at T0 + 10 days and a minute the next ten. NC: an open that restamps the retained
/// suffix (the stamps read back left out) cuts nothing until 7 days after the last restart.
#[test]
fn an_age_rule_cuts_by_when_a_record_was_first_seen_across_restarts() {
    let mut fx = Fx::new("held-ages", false);
    let r = rules(None, Some(7 * DAY), None, 7 * DAY, false);
    let mut log = fx.create();
    log.set_retention(r.clone()).unwrap();
    fx.batch(&mut log, &[EV; 10]);
    assert_eq!(log.retain_by_rules(T0).unwrap().base, 0);
    drop(log);
    let mut log = fx.open();
    log.set_retention(r.clone()).unwrap();
    fx.batch(&mut log, &[EV; 10]);
    assert_eq!(log.retain_by_rules(T0 + 3 * DAY).unwrap().base, 0);
    drop(log);
    let mut log = fx.open();
    log.set_retention(r.clone()).unwrap();
    assert_eq!(
        log.retain_by_rules(T0 + 6 * DAY).unwrap().base,
        0,
        "nothing is 7 days old"
    );
    let tick = log.retain_by_rules(T0 + 7 * DAY + MINUTE).unwrap();
    assert_eq!(
        (tick.base_before, tick.base),
        (0, 10),
        "the ten first seen at T0, whatever the restarts since"
    );
    drop(log);
    let mut log = fx.open();
    log.set_retention(r).unwrap();
    assert_eq!(log.retain_by_rules(T0 + 9 * DAY).unwrap().base, 10);
    assert_eq!(
        log.retain_by_rules(T0 + 10 * DAY + MINUTE).unwrap().base,
        20,
        "the ten first seen at T0 + 3 days"
    );
}

/// A CRASH THAT LOSES STAMPED RECORDS NEVER AGES THE ONES AFTER IT: a tick at T0 stamps
/// twenty records, the second batch of which (15 to 19) a crash then tears away; the log
/// reopened takes five new records at those offsets, and is reopened again before any tick
/// sees them. A tick at T0 + 7 days and a minute cuts the fifteen first seen at T0, never
/// the five no tick had seen. NC: stamps read back that are not cut to the head recovered
/// (and, with the cut kept in memory only, the second open's) age the new five 7 days and
/// cut them too.
#[test]
fn a_crash_that_loses_stamped_records_never_ages_the_ones_after_it() {
    let mut fx = Fx::new("held-stamps-lost", false);
    let r = rules(None, Some(7 * DAY), None, 7 * DAY, false);
    let mut log = fx.create();
    log.set_retention(r.clone()).unwrap();
    fx.batch(&mut log, &[EV; 15]);
    fx.batch(&mut log, &[EV; 5]);
    assert_eq!(log.retain_by_rules(T0).unwrap().base, 0);
    drop(log);
    // The power went before the second batch's bytes were all on the disk: its sealing
    // record is torn, so the open cuts the batch at its start.
    let len = std::fs::metadata(&fx.path).unwrap().len();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&fx.path)
        .unwrap()
        .set_len(len - 3)
        .unwrap();
    let mut log = fx.open();
    assert_eq!(
        log.head(),
        Offset(15),
        "PRECONDITION: the torn batch is cut"
    );
    assert_eq!(fx.batch(&mut log, &[EV; 5]), vec![15, 16, 17, 18, 19]);
    drop(log);
    let mut log = fx.open();
    log.set_retention(r).unwrap();
    let tick = log.retain_by_rules(T0 + 7 * DAY + MINUTE).unwrap();
    assert_eq!(tick.base, 15, "only the records first seen at T0");
    assert_eq!(
        log.read_from(Offset(0))
            .iter()
            .map(|r| r.seq.0)
            .collect::<Vec<_>>(),
        vec![15, 16, 17, 18, 19],
        "the five no tick had seen are kept"
    );
}

/// A REWRITE KEEPS THE STAMPS: a store due a rewrite (its length set past the bound by
/// hand) is written whole at the next tick, the tick's stamps in its snapshot, and the open
/// after it reads every record's age back: a tick at T0 + 7 days and a minute cuts the four
/// first seen at T0, at T0 + 8 days and a minute the next four. NC: a snapshot without the
/// stamps reads back none, and nothing is cut until 7 days after the restart.
#[test]
fn a_rewritten_store_keeps_the_ticks_stamps() {
    let mut fx = Fx::new("held-rewrite-stamps", false);
    let r = rules(None, Some(7 * DAY), None, 7 * DAY, false);
    let mut log = fx.create();
    log.set_retention(r.clone()).unwrap();
    for day in 0..3 {
        fx.batch(&mut log, &[EV; 4]);
        if day == 2 {
            log.held_store_mut().len += 16 << 20;
            assert!(log.held_store_mut().rewrite_due(), "PRECONDITION");
        }
        assert_eq!(log.retain_by_rules(T0 + day * DAY).unwrap().base, 0);
    }
    assert!(
        std::fs::metadata(held_path(&fx.path)).unwrap().len() < 1 << 20,
        "PRECONDITION: the store was rewritten"
    );
    drop(log);
    let mut log = fx.open();
    log.set_retention(r).unwrap();
    assert_eq!(log.retain_by_rules(T0 + 7 * DAY + MINUTE).unwrap().base, 4);
    assert_eq!(log.retain_by_rules(T0 + 8 * DAY + MINUTE).unwrap().base, 8);
}

/// A TORN OR ZEROED TAIL is cut at its frame (an append a crash interrupted holds nothing a
/// landed compaction needs); damage inside the store refuses the open, naming the file.
#[test]
fn a_torn_held_tail_is_cut_and_damage_refuses_naming_the_file() {
    let mut fx = Fx::new("held-torn", false);
    let mut log = seed(&mut fx);
    let want = snap(&mut log);
    drop(log);
    let held = held_path(&fx.path);
    let whole = std::fs::read(&held).unwrap();
    let frame = Frame::new(b"Tsomething long enough".to_vec())
        .encode()
        .unwrap();
    for (how, tail) in [
        (
            "a frame cut short",
            frame.get(..frame.len() - 3).unwrap().to_vec(),
        ),
        ("a header cut short", frame.get(..5).unwrap().to_vec()),
        ("zeros", vec![0u8; 40]),
    ] {
        let mut b = whole.clone();
        b.extend_from_slice(&tail);
        std::fs::write(&held, &b).unwrap();
        let mut log = fx
            .try_open()
            .unwrap_or_else(|e| panic!("{how}: the open refused: {e}"));
        assert_eq!(snap(&mut log), want, "{how}");
        drop(log);
        assert_eq!(
            std::fs::read(&held).unwrap(),
            whole,
            "{how}: cut at the frame"
        );
    }
    let mut b = whole.clone();
    let mid = HEADER_SIZE + 4;
    b[mid] ^= 0xff;
    std::fs::write(&held, &b).unwrap();
    let err = fx.try_open().err().expect("damage refuses");
    assert!(err.to_string().contains(".held"), "{err}");
    std::fs::write(&held, &whole).unwrap();
    assert_eq!(snap(&mut fx.open()), want, "the store put back opens");
}

/// A RULES CHANGE KEEPS THE HELD MAIL READABLE: a tick carried an unread mail at 1 below a
/// cut at 3; the rules set again with the stream's bytes doubled and a new hold class beside
/// them, the mail still reads from 0 with no `retained-from=`, its class whole from where it
/// was, while a read of the new class below the base is told where it starts — live, and
/// across a restart (the rules set again). NC: every class reset to the base by the change
/// refuses the mail's read `retained-from=3`.
#[test]
fn a_rules_change_keeps_the_held_mail_readable() {
    let mut fx = Fx::new("held-retune", false);
    let mut log = fx.create();
    log.set_retention(rules(Some(rb(EV)), None, None, 7 * DAY, false))
        .unwrap();
    fx.batch(&mut log, &[EV, MAIL, EV, EV]);
    assert_eq!(log.retain_by_rules(T0).unwrap().base, 3);
    let retuned = || {
        let mut r: Vec<Rule> = rules(Some(2 * rb(EV)), None, None, 7 * DAY, false)
            .rules()
            .to_vec();
        r.push(Rule::hold("/f/F/x/>").unwrap());
        Retention::new(r, Rule::default_hold()).unwrap()
    };
    log.set_retention(retuned()).unwrap();
    let mail = filter("/f/F/in/>");
    for pass in ["live", "after a restart"] {
        let head = log.head().0;
        let got = log
            .fetch_checked(0, &mail, 10, 100, head)
            .map(|(p, _)| p.iter().map(|r| r.seq.0).collect::<Vec<_>>());
        assert_eq!(got, Ok(vec![1]), "{pass}: the held mail reads from 0");
        assert_eq!(log.oldest(&filter("/f/F/x/>")), Offset(3), "{pass}");
        drop(log);
        log = fx.open();
        log.set_retention(retuned()).unwrap();
    }
}

/// THE REWRITE: a hold class that churns (each tick carries a large mail and gives up the
/// oldest) grows the file past twice what it carries, and the next tick writes it whole:
/// the file stays within a small multiple of what it holds, and reads back the same.
#[test]
fn a_churning_store_is_rewritten_and_reads_back_the_same() {
    let mut fx = Fx::new("held-rewrite", false);
    let mut log = fx.create();
    let big = vec![7u8; 64 * 1024];
    let mail_bytes = 2 * record_bytes(&rec_of(MAIL, &big));
    log.set_retention(rules(Some(rb(EV)), None, Some(mail_bytes), 7 * DAY, false))
        .unwrap();
    let mut largest = 0u64;
    for i in 0..48u64 {
        fx.seq += 1;
        log.stage_publish(1, fx.seq, MAIL.into(), big.clone())
            .unwrap();
        fx.seq += 1;
        log.stage_publish(1, fx.seq, EV.into(), BODY.to_vec())
            .unwrap();
        fx.seq += 1;
        log.stage_publish(1, fx.seq, EV.into(), BODY.to_vec())
            .unwrap();
        log.commit_batch().unwrap();
        log.retain_by_rules(T0 + i * MINUTE).unwrap();
        largest = largest.max(std::fs::metadata(held_path(&fx.path)).unwrap().len());
    }
    assert_eq!(log.stats().held_records, 2);
    let live = log.stats().held_bytes;
    assert!(
        largest <= 2 * (live + 64 * 1024) + (1 << 20) + 2 * 70 * 1024,
        "the file grew to {largest} bytes for {live} held"
    );
    assert!(
        largest > 1 << 20,
        "PRECONDITION: the churn passed the slack"
    );
    let want = snap(&mut log);
    drop(log);
    assert_eq!(snap(&mut fx.open()), want);
}

/// WITHOUT RULES nothing changes but the refusal: no held store is written, a tick does
/// nothing, and a read below a base a caller compacted to is clamped by `fetch` and refused
/// by `fetch_checked` with the base.
#[test]
fn a_log_without_rules_keeps_no_held_store() {
    let mut fx = Fx::new("held-none", false);
    let mut log = fx.create();
    fx.batch(&mut log, &[MAIL, EV, EV]);
    assert_eq!(log.retain_by_rules(T0).unwrap().base, 0);
    assert_eq!(log.retain_before(Offset(2)).unwrap(), 2);
    assert!(std::fs::symlink_metadata(held_path(&fx.path)).is_err());
    let all = filter("/>");
    assert_eq!(log.oldest(&all), Offset(2));
    let (page, _) = log.fetch(0, &all, 10, 100, 3);
    assert_eq!(page[0].seq, Offset(2), "fetch clamps");
    assert_eq!(
        log.fetch_checked(0, &all, 10, 100, 3).map(|(p, _)| p.len()),
        Err(Offset(2))
    );
    assert_eq!(
        log.fetch_checked(0, &all, 0, 100, 3).unwrap().1,
        0,
        "the head query"
    );
}

/// A COMPACTION A CALLER ASKS FOR on a log its rules retain carries what they hold below
/// the floor it names, and its state, as a tick's does.
#[test]
fn a_callers_compaction_carries_what_the_rules_hold() {
    let mut fx = Fx::new("held-caller", false);
    let mut log = fx.create();
    log.set_retention(rules(None, None, None, 7 * DAY, true))
        .unwrap();
    fx.batch(&mut log, &[MAIL, PRESENCE, EV, EV]);
    assert_eq!(log.retain_before(Offset(3)).unwrap(), 3);
    assert_eq!(read(&log, "/f/F/in/>"), vec![0]);
    let last = last_of(&mut log, &filter("/f/F/pub/*/*/presence"), 10, 100);
    assert_eq!(last.len(), 1, "its state carried");
}

/// A LOG CREATED where one with a held store was deleted carries none of it.
#[test]
fn a_created_log_carries_no_predecessors_held_store() {
    let mut fx = Fx::new("held-created", false);
    let log = seed(&mut fx);
    drop(log);
    assert!(std::fs::symlink_metadata(held_path(&fx.path)).is_ok());
    std::fs::remove_file(&fx.path).unwrap();
    let log = fx.open();
    assert_eq!(log.retention(), None);
    assert_eq!(log.stats().held_records, 0);
    assert!(std::fs::symlink_metadata(held_path(&fx.path)).is_err());
}
