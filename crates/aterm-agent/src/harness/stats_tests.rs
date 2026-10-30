// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::upgrade_drive::{self, Instances, Opts, Report};
use crate::supervise::ScratchDir;
use crate::supervise::approvals::Row;
use crate::supervise::config::CONTINUE_TEXT;
use crate::supervise::journal::ATTENTION_KEPT;
use crate::supervise::policy::approval::{
    RULE_ALLOW_ONCE, RULE_ANSWER_LONG_TERM, RULE_LIMIT_WAIT, RULE_READ_ONLY,
};
use crate::supervise::policy::question::RULE_ANSWER_RECOMMENDED;
use crate::supervise::policy::turn_end::{
    LAND_ANSWER, LAND_NUDGE_COMMITTED_BEFORE, RULE_ACCOUNT, RULE_ANSWER, RULE_API_BACK,
    RULE_API_CUTOFF, RULE_API_RETRY, RULE_COMPACT, RULE_CONSENT, RULE_CONTINUE, RULE_DONE_CHECK,
    RULE_HOST_TURN, RULE_LAND_ANSWER, RULE_LAND_NUDGE, RULE_LIMIT_RESUME, RULE_LOGIN,
    RULE_MODEL_FALLBACK, RULE_OUTAGE_RESUME, RULE_SUGGESTION, RULE_USAGE_PROBE, WAKE_KEPT,
};

fn row(ts: i64, sid: &str, rule_id: &str, outcome: Outcome, command: &str, reason: &str) -> String {
    Row {
        rule_id,
        outcome,
        command,
        reason,
        box_seq: 1,
    }
    .to_json(ts, Some(sid))
}

fn typed(ts: i64, rule: &str, command: &str) -> String {
    row(
        ts,
        "s-1",
        rule,
        Outcome::Typed,
        command,
        "the turn-end policy",
    )
}

fn said(ts: i64, rule: &str, outcome: Outcome, reason: &str) -> String {
    row(ts, "s-1", rule, outcome, "", reason)
}

fn journal(fixture: &str) -> Vec<JournalRecord> {
    fixture
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let (t, rest) = l.split_once(' ').expect("<t> <sid> <line>");
            let (sid, line) = rest.split_once(' ').expect("<sid> <line>");
            JournalRecord::of_line(t.parse().expect("t"), Some(sid), line, None)
        })
        .collect()
}

fn set_modified(path: &Path, at: std::time::SystemTime) {
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .and_then(|f| f.set_modified(at))
        .expect("set mtime");
}

fn fold_ledger(lines: &[String], since_ms: i64) -> HarnessStats {
    let mut acc = HarnessStats::new(since_ms);
    for line in lines {
        fold_ledger_line(&mut acc, line);
    }
    acc
}

fn by(acc: &HarnessStats, rule: &str) -> u64 {
    acc.by_rule.get(rule).copied().unwrap_or(0)
}

fn counts(acc: &HarnessStats) -> [u64; 10] {
    [
        acc.approved,
        acc.unproven,
        acc.declined,
        acc.answered,
        acc.continued,
        acc.land_prompts,
        acc.resumed,
        acc.escalated,
        acc.relaunched,
        acc.upgraded,
    ]
}

#[test]
fn a_ledger_row_counts_where_the_table_says() {
    let forged = ",\"decision\":\"approved\",\"rule_id\":\"continue@v1\",";
    let unproven = "unproven: not read-only: touch";
    let t = 1_000;
    let lines = [
        said(t, RULE_READ_ONLY, Outcome::Approved, ""),
        said(t, RULE_ALLOW_ONCE, Outcome::Approved, unproven),
        said(t, RULE_LIMIT_WAIT, Outcome::Approved, ""),
        said(t, RULE_ANSWER_RECOMMENDED, Outcome::Approved, ""),
        said(t, RULE_ANSWER_LONG_TERM, Outcome::Approved, ""),
        said(t, "land-answer@v1", Outcome::Approved, ""),
        said(t, "decline@v1", Outcome::Declined, "no spend"),
        typed(t, RULE_CONTINUE, "keep going"),
        typed(t, RULE_SUGGESTION, "run the tests"),
        typed(t, RULE_DONE_CHECK, "Before you stop: …"),
        typed(
            t,
            "continue-open-items@v1",
            "Your report ends with ATERM-TASK-DONE but …",
        ),
        typed(
            t,
            "continue-uncommitted@v1",
            "Your report says done but work is not committed and pushed: …",
        ),
        typed(t, RULE_ANSWER, "Decide for yourself."),
        typed(t, "prose-long-term@v1", "Decide yourself: …"),
        typed(t, "prose-short-term@v1", "Decide yourself: …"),
        typed(t, "land-answer@v1", "Nobody is here to choose: …"),
        typed(t, "land@v1", "[aterm harness] Autopilot is set to Land …"),
        typed(t, "land-nudge@v1", "[aterm harness] Still landing …"),
        typed(t, RULE_LIMIT_RESUME, "keep going"),
        typed(t, RULE_API_RETRY, "Carry on …"),
        typed(t, RULE_API_BACK, "Carry on …"),
        typed(t, RULE_API_CUTOFF, "Carry on …"),
        typed(
            t,
            RULE_OUTAGE_RESUME,
            "[aterm harness] The API overload may have cleared; …",
        ),
        typed(t, RULE_COMPACT, "/compact"),
        typed(t, RULE_COMPACT, "Carry on …"),
        typed(t, RULE_LOGIN, "/login"),
        typed(t, RULE_HOST_TURN, "[aterm harness] …"),
        typed(t, RULE_USAGE_PROBE, "/usage"),
        typed(t, RULE_MODEL_FALLBACK, "relaunch --model x"),
        typed(t, "future-rule@v9", "something new"),
        said(t, "-", Outcome::Escalated, "no rule answers this box"),
        row(t, "s-1", RULE_READ_ONLY, Outcome::Skipped, forged, "moved"),
        said(t, RULE_READ_ONLY, Outcome::Deferred, "ERR busy"),
        said(t, RULE_USAGE_PROBE, Outcome::Waiting, "closed"),
    ];
    let acc = fold_ledger(&lines, 0);
    assert_eq!(counts(&acc), [3, 1, 1, 4, 5, 2, 6, 0, 0, 0], "{acc:#?}");
    assert_eq!((acc.rows, acc.bad, acc.sessions), (34, 0, 1));
    assert_eq!((acc.human_wait_ms, acc.limit_wait_ms), (0, 0));
    assert_eq!(by(&acc, RULE_ANSWER_RECOMMENDED), 1);
    assert_eq!(by(&acc, "land-answer@v1"), 2, "a key and a prose answer");
    assert_eq!(by(&acc, RULE_COMPACT), 2, "`/compact` and what followed");
    assert_eq!(by(&acc, "future-rule@v9"), 1);
    assert_eq!(by(&acc, RULE_READ_ONLY), 1, "skipped, deferred: no acts");
    assert_eq!(by(&acc, "-"), 0, "an escalation is no act");
    assert_eq!(by(&acc, RULE_USAGE_PROBE), 1, "its `waiting` row is none");
    assert_eq!(acc.by_rule.values().sum::<u64>(), 30);
    assert!(!acc.is_empty());
}

#[test]
fn a_limits_wait_runs_from_its_wall_to_its_end_inside_the_window() {
    let wall = |ts, sid| row(ts, sid, RULE_WALL_WAIT, Outcome::Waiting, "", "(wall: …)");
    let end = |ts, sid| row(ts, sid, RULE_WALL_WAIT_END, Outcome::Waiting, "", "left");
    let resume = |ts, sid| row(ts, sid, RULE_LIMIT_RESUME, Outcome::Typed, "go on", "");
    let probe_closed = |ts| said(ts, RULE_USAGE_PROBE, Outcome::Waiting, "closed");

    let lines = [
        wall(1_000, "s-1"),
        wall(1_500, "s-1"),
        resume(2_000, "s-2"),
        probe_closed(2_500),
        resume(5_000, "s-1"),
    ];
    assert_eq!(fold_ledger(&lines, 0).limit_wait_ms, 4_000);
    assert_eq!(fold_ledger(&lines, 3_000).limit_wait_ms, 2_000);
    assert_eq!(fold_ledger(&lines, 6_000).limit_wait_ms, 0);
    let lines = [
        wall(1_000, "s-1"),
        end(1_250, "s-1"),
        wall(2_000, "s-1"),
        probe_closed(9_000),
    ];
    assert_eq!(fold_ledger(&lines, 0).limit_wait_ms, 250);
    let lines = [
        wall(1_000, "s-1"),
        wall(1_100, "s-2"),
        resume(1_400, "s-2"),
        resume(1_600, "s-1"),
    ];
    assert_eq!(fold_ledger(&lines, 0).limit_wait_ms, 300 + 600);
}

const JOURNAL: &str = "
    1000 s-1 ESCALATED seq=10 attention=OK mail=OK 7 off=91
    # Ends of points that raised no badge.
    1500 s-1 CLEARED seq=11 survey
    1600 s-1 CLEARED seq=11 usage panel
    1700 s-1 EXCEPTION seq=11 kind=spend when=later why => act
    1800 s-1 CLEARED seq=11 exception=spend after=1s the agent working
    # Another session's badge.
    2000 s-2 CLEARED seq=3 box attention=OK
    4000 s-1 CLEARED seq=14 box attention=OK
    # A limit episode, escalated twice, closed by its `attention=`.
    5000 s-1 ESCALATED seq=20 attention=OK mail=skipped: no fabric (fabric=absent)
    5500 s-1 ESCALATED seq=21 attention=OK mail=OK 8 off=92
    6000 s-1 CLEARED seq=22 attention=OK answered after a busy spell
    # Another supervisor answers this one: nobody was asked.
    7000 s-1 ESCALATED seq=30 attention=skipped: another supervisor (s-9) answers this session mail=skipped: the same
    9000 s-1 CLEARED seq=31 turn-end attention=OK
    # A dialog the loop answered, and the lines the ledger already has.
    9100 s-1 CHOSE seq=40 rule=answer-recommended@v1 policy=recommended Which colour? → Blue
    9200 s-1 APPROVED seq=41 git log
    9300 s-1 DECLINED seq=42 rule=decline@v1 a box => text
    9400 s-1 CONTINUED seq=43 rule=continue@v1 keep going
    9500 s-1 TYPED seq=44 rule=context-compact@v1 /compact
    9600 s-1 UNPROVEN seq=41 rule=allow-once@v1 unproven: not read-only: touch
";

#[test]
fn journal_escalations_wait_on_a_person_until_their_badge_clears() {
    let recs = journal(JOURNAL);
    assert_eq!(recs.len(), 18);
    let fold = |since_ms| {
        let mut acc = HarnessStats::new(since_ms);
        fold_journal(&mut acc, &recs);
        acc
    };
    let acc = fold(0);
    assert_eq!(counts(&acc), [0, 0, 0, 1, 0, 0, 0, 3, 0, 0], "{acc:#?}");
    assert_eq!(acc.human_wait_ms, 3_000 + 1_000, "{acc:#?}");
    assert!(acc.by_rule.is_empty(), "a journal line is no ledger act");
    assert_eq!((acc.rows, acc.sessions), (18, 2));
    let acc = fold(4_500);
    assert_eq!(
        (acc.escalated, acc.human_wait_ms, acc.answered),
        (2, 1_000, 1)
    );
    let acc = fold(3_000);
    assert_eq!((acc.escalated, acc.human_wait_ms), (2, 1_000 + 1_000));
}

#[test]
fn a_failed_badge_set_opens_no_person_wait() {
    let recs = journal(
        "1000 s-1 ESCALATED seq=10 attention=ERR denied mail=skipped: no fabric\n\
         2000 s-1 CLEARED seq=11 box attention=none\n\
         3000 s-1 ESCALATED seq=12 attention=OK mail=skipped: no fabric\n\
         5000 s-1 CLEARED seq=13 box attention=OK",
    );
    let mut acc = HarnessStats::new(0);
    fold_journal(&mut acc, &recs[..2]);
    assert_eq!((acc.escalated, acc.human_wait_ms), (0, 0), "{acc:#?}");
    fold_journal(&mut acc, &recs[2..]);
    assert_eq!((acc.escalated, acc.human_wait_ms), (1, 2_000), "{acc:#?}");
}

#[test]
fn a_failed_badge_clear_keeps_the_wait_open_until_a_confirmed_clear() {
    for reply in ["ERR denied", "ERR timeout", "OKAY", ""] {
        let recs = journal(&format!(
            "1000 s-1 ESCALATED seq=10 attention=OK mail=OK\n\
             2000 s-1 CLEARED seq=11 box attention={reply}\n\
             5000 s-1 CLEARED seq=12 box attention=OK"
        ));
        let mut acc = HarnessStats::new(0);
        fold_journal(&mut acc, &recs[..2]);
        assert_eq!(acc.human_wait_ms, 0, "{reply:?}: {acc:#?}");
        assert!(acc.open.human.contains_key("s-1"), "{reply:?}: {acc:#?}");
        fold_journal(&mut acc, &recs[2..]);
        assert_eq!(acc.human_wait_ms, 4_000, "{reply:?}: {acc:#?}");
    }
}

#[test]
fn a_failed_frozen_clear_keeps_the_wait_open_until_a_confirmed_clear() {
    for reply in ["ERR denied", "ERR timeout", "OKAY", ""] {
        let recs = journal(&format!(
            "1000 s-1 ESCALATED seq=10 attention=OK mail=OK\n\
             2000 s-1 FROZEN seq=11 input=stalled attention={reply} mail=skipped: no fabric\n\
             5000 s-1 FROZEN seq=12 input=stalled attention=OK mail=OK"
        ));
        let mut acc = HarnessStats::new(0);
        fold_journal(&mut acc, &recs[..2]);
        assert_eq!(acc.human_wait_ms, 0, "{reply:?}: {acc:#?}");
        assert!(acc.open.human.contains_key("s-1"), "{reply:?}: {acc:#?}");
        fold_journal(&mut acc, &recs[2..]);
        assert_eq!(acc.human_wait_ms, 4_000, "{reply:?}: {acc:#?}");
    }
}

const WAITS: &str = "
    # The loop's end ends a wait no clear closed: counted nothing, and the
    # next escalation opens its own (not the whole gap).
    1000 s-1 ESCALATED seq=10 attention=OK mail=OK 7 off=91
    2000 s-1 EXIT stopped
    5000 s-1 ESCALATED seq=12 attention=OK mail=OK 8 off=92
    6000 s-1 CLEARED seq=13 box attention=OK
    # A budget's end too.
    7000 s-1 ESCALATED seq=20 attention=OK mail=OK
    7500 s-1 TIMEOUT
    # A badge handed over: kept for the next loop, which adopts it and
    # counts its wait from there.
    10000 s-2 ESCALATED seq=30 attention=OK mail=OK
    11000 s-2 KEPT attention for the loop that takes the session over
    11000 s-2 EXIT stopped
    12000 s-2 ADOPTED seq=31 attention=claude needs approval: rm -rf x (not read-only)
    15000 s-2 CLEARED seq=32 box attention=OK
    # A loop that died with its badge up (no end line): the next loop's
    # hidden clear, read no seq, takes it down.
    20000 s-3 ESCALATED seq=40 attention=OK mail=OK
    23000 s-3 CLEARED seq=- hidden attention=OK (an entry of ours may sit under another owner's)
    # An ask a stall withheld: no badge, nobody asked.
    30000 s-4 ESCALATED seq=50 attention=skipped: the same status says the worker is not reading its input mail=skipped: no --inbox and no $ATERM_PARENT_SESSION_ID
    31000 s-4 CLEARED seq=51 box attention=none
    # A limit episode whose retry hit the wall again: one escalation, its
    # wait counted while each of its badges stood.
    40000 s-5 ESCALATED seq=60 attention=OK mail=OK 9 off=93
    41000 s-5 CLEARED seq=61 attention=OK the worker works again: the notice said it would continue by itself
    42000 s-5 ESCALATED seq=62 attention=OK mail=skipped: the retry hit the wall again, the episode of seq=60
    44000 s-5 CLEARED seq=63 attention=OK answered after a busy spell
    # A limit episode adopted from a previous loop, its badge set again:
    # one wait, no escalation of this loop's.
    50000 s-6 ADOPTED seq=70 attention=limited: You've hit your session limit
    50100 s-6 ESCALATED seq=70 attention=OK mail=skipped: adopted from a previous watcher
    53000 s-6 CLEARED seq=72 attention=OK answered after a busy spell
    # Overlapping badges: the goal's note over the turn-end policy's. The
    # turn-end badge's clear finds the goal's text on the loop's one entry
    # and keeps it; the goal's clear takes it down.
    60000 s-7 ESCALATED seq=80 attention=OK mail=OK
    61000 s-7 ESCALATED seq=80 attention=OK mail=OK
    62000 s-7 CLEARED seq=82 turn-end attention=kept
    65000 s-7 CLEARED seq=90 goal attention=OK
";

#[test]
fn a_persons_wait_runs_while_a_badge_of_the_loops_stands() {
    let recs = journal(WAITS);
    let fold = |recs: &[JournalRecord], since_ms| {
        let mut acc = HarnessStats::new(since_ms);
        fold_journal(&mut acc, recs);
        acc
    };
    let per = |sid: &str, since_ms| {
        let mine: Vec<JournalRecord> = recs
            .iter()
            .filter(|r| r.sid.as_deref() == Some(sid))
            .cloned()
            .collect();
        let acc = fold(&mine, since_ms);
        (acc.escalated, acc.human_wait_ms)
    };
    assert_eq!(per("s-1", 0), (3, 1_000), "only the last pair counts");
    assert_eq!(per("s-2", 0), (1, 3_000), "from the adoption on");
    assert_eq!(per("s-3", 0), (1, 3_000), "the hidden clear ends it");
    assert_eq!(per("s-4", 0), (0, 0), "a withheld ask");
    assert_eq!(
        per("s-5", 0),
        (1, 1_000 + 2_000),
        "the retry: no escalation"
    );
    assert_eq!(per("s-6", 0), (0, 3_000), "adopted: no escalation");
    assert_eq!(per("s-7", 0), (2, 5_000), "kept: one wait");
    let acc = fold(&recs, 0);
    assert_eq!(
        (acc.escalated, acc.human_wait_ms),
        (8, 1_000 + 3_000 + 3_000 + 3_000 + 3_000 + 5_000),
        "{acc:#?}"
    );
    let acc = fold(&recs, 41_500);
    assert_eq!(
        (acc.escalated, acc.human_wait_ms),
        (2, 2_000 + 3_000 + 5_000)
    );

    let unended: Vec<JournalRecord> = recs
        .iter()
        .filter(|r| {
            r.sid.as_deref() == Some("s-1") && !matches!(r.kind.as_str(), "exit" | "timeout")
        })
        .cloned()
        .collect();
    assert_eq!(fold(&unended, 0).human_wait_ms, 5_000);
    let down = journal(
        "
        60000 s-7 ESCALATED seq=80 attention=OK mail=OK
        61000 s-7 ESCALATED seq=80 attention=OK mail=OK
        62000 s-7 CLEARED seq=82 turn-end attention=OK
        65000 s-7 CLEARED seq=90 goal attention=none
        ",
    );
    assert_eq!(fold(&down, 0).human_wait_ms, 2_000);
}

#[test]
fn the_folds_words_are_the_writers_consts() {
    use crate::supervise::journal::{
        CLEARED_BOX, CLEARED_GOAL, CLEARED_HIDDEN, CLEARED_MEMORY, CLEARED_STALE, CLEARED_TURN_END,
        ESCALATED_BEHIND,
    };
    for word in [
        CLEARED_BOX,
        CLEARED_TURN_END,
        CLEARED_GOAL,
        CLEARED_MEMORY,
        CLEARED_STALE,
        CLEARED_HIDDEN,
    ] {
        assert!(
            takes_badge_down(&format!("seq=4 {word} attention=OK")),
            "{word}"
        );
        assert!(
            takes_badge_down(&format!("seq=- {word} attention=OK")),
            "{word}"
        );
        assert!(!takes_badge_down(&format!(
            "seq=4 {word} attention={ATTENTION_KEPT}"
        )));
    }
    assert!(takes_badge_down(
        "seq=4 memory attention=OK: the measure is no longer acted on"
    ));
    assert!(takes_badge_down("attention=OK the worker works again"));
    assert!(!takes_badge_down("seq=4 survey"));
    assert!(!takes_badge_down("seq=4 usage panel"));
    assert!(!takes_badge_down(
        "seq=4 exception=spend after=1s the agent working"
    ));
    assert!(
        !takes_badge_down("seq=4 boxes attention=OK"),
        "a reworded badge word"
    );
    let behind = format!("{ATTENTION_SKIPPED}{ESCALATED_BEHIND} (s-9) answers this session");
    assert_eq!(escalation_of(&behind), Escalation::Withheld);
    let retry = format!("attention=OK again={AGAIN_RETRY}, the episode of seq=60");
    assert_eq!(escalation_of(&retry), Escalation::Again);
    let adopted = format!("attention=OK again={AGAIN_ADOPTED}");
    assert_eq!(escalation_of(&adopted), Escalation::Again);
    let unparked = format!("attention=OK again={AGAIN_UNPARKED}");
    assert_eq!(escalation_of(&unparked), Escalation::Again);
    let mailed_retry = format!("attention=OK mail=skipped: {AGAIN_RETRY}, the episode of seq=60");
    assert_eq!(escalation_of(&mailed_retry), Escalation::Again);
    assert_eq!(escalation_of("attention=OK"), Escalation::Asked);
    assert_eq!(
        escalation_of("attention=OK mail=OK 7 off=91"),
        Escalation::Asked
    );
    assert_eq!(
        escalation_of("attention=OK mail=skipped: no fabric (fabric=absent)"),
        Escalation::Asked
    );
    let adopted_line = JournalRecord::of_line(
        1,
        Some("s-1"),
        &format!("{ADOPTED} seq=3 attention=x"),
        None,
    );
    assert!(is_adopted(&adopted_line));
    let other = JournalRecord::of_line(1, Some("s-1"), "ADOPTEDX seq=3 attention=x", None);
    assert!(!is_adopted(&other));
}

const ENDS: &str = "
    # A stall's hold takes the point's badge down on its FROZEN line alone.
    1000 s-1 ESCALATED seq=10 attention=OK mail=OK 7 off=91
    3000 s-1 FROZEN seq=11 input=stalled wait_ms=161000 bytes=1 rss_mb=39731 attention=OK mail=OK 8 off=92
    9000 s-1 EXIT session gone
    # The entry carries a limit's badge, which stands under the hold: kept,
    # and the wait goes on to the limit's own clear.
    10000 s-2 ESCALATED seq=20 attention=OK mail=OK
    11000 s-2 FROZEN seq=21 input=stalled wait_ms=161000 bytes=1 rss_mb=- attention=kept mail=OK
    14000 s-2 CLEARED seq=22 attention=OK answered after a busy spell
    # Behind another supervisor the hold touches nothing: the adopted
    # badge's wait goes on to its clear.
    20000 s-3 ADOPTED seq=30 attention=claude needs approval: rm -rf x (not read-only)
    21000 s-3 FROZEN seq=31 input=stalled wait_ms=- bytes=- rss_mb=- attention=skipped: another supervisor (gui:9) answers this session mail=skipped: another supervisor (gui:9) answers this session
    25000 s-3 CLEARED seq=32 box attention=OK
    # The turn-end policy's badge at the loop's end: its CLEARED line, then
    # the end.
    30000 s-4 ESCALATED seq=40 attention=OK mail=OK
    34000 s-4 CLEARED seq=40 turn-end attention=OK
    34000 s-4 EXIT stopped
    # An Off park takes the badge down as it starts; the point decided
    # afresh after it raises its own.
    40000 s-5 ESCALATED seq=50 attention=OK mail=OK
    41000 s-5 PARKED seq=50 autopilot=off: the harness keeps its hands off this session (nothing pressed, typed or escalated) until its word changes
    41000 s-5 CLEARED seq=50 box attention=OK
    50000 s-5 UNPARKED seq=51 autopilot=cruise after 9000 ms
    50100 s-5 ESCALATED seq=51 attention=OK mail=OK
    52000 s-5 CLEARED seq=52 box attention=OK
";

#[test]
fn a_stalls_hold_a_parks_start_and_a_loops_end_close_a_wait() {
    let recs = journal(ENDS);
    let fold = |recs: &[JournalRecord]| {
        let mut acc = HarnessStats::new(0);
        fold_journal(&mut acc, recs);
        acc
    };
    let mine = |sid: &str| -> Vec<JournalRecord> {
        recs.iter()
            .filter(|r| r.sid.as_deref() == Some(sid))
            .cloned()
            .collect()
    };
    let per = |sid: &str| {
        let acc = fold(&mine(sid));
        (acc.escalated, acc.human_wait_ms)
    };
    assert_eq!(per("s-1"), (1, 2_000), "the hold ends it");
    assert_eq!(per("s-2"), (1, 4_000), "kept: the limit's clear ends it");
    assert_eq!(per("s-3"), (0, 5_000), "skipped: its clear ends it");
    assert_eq!(per("s-4"), (1, 4_000), "the end's clear");
    assert_eq!(per("s-5"), (2, 1_000 + 1_900), "the park is no wait");

    let unfrozen: Vec<JournalRecord> = mine("s-1")
        .into_iter()
        .filter(|r| !r.line.starts_with("FROZEN"))
        .collect();
    assert_eq!(fold(&unfrozen).human_wait_ms, 0);
    let uncleared: Vec<JournalRecord> = mine("s-4")
        .into_iter()
        .filter(|r| r.kind != "cleared")
        .collect();
    assert_eq!(fold(&uncleared).human_wait_ms, 0);
    let parked: Vec<JournalRecord> = mine("s-5")
        .into_iter()
        .filter(|r| !(r.kind == "cleared" && r.t == 41_000))
        .collect();
    assert_eq!(fold(&parked).human_wait_ms, 12_000);
    let reworded = journal(
        "
        1000 s-1 ESCALATED seq=10 attention=OK mail=OK
        3000 s-1 FROZE seq=11 input=stalled attention=OK mail=OK
        9000 s-1 CLEARED seq=12 box attention=OK
        ",
    );
    assert_eq!(fold(&reworded).human_wait_ms, 8_000);
}

#[test]
fn the_frozen_line_is_read_with_the_writers_consts() {
    use crate::supervise::journal::ESCALATED_BEHIND;
    let frozen = |attention: &str| {
        JournalRecord::of_line(
            1,
            Some("s-1"),
            &format!(
                "{FROZEN} seq=4 input=stalled wait_ms=161000 bytes=1 rss_mb=39731 {attention} \
                 mail=OK 8 off=92"
            ),
            None,
        )
    };
    assert!(frozen_takes_badge_down(&frozen("attention=OK")));
    assert!(frozen_takes_badge_down(&frozen("attention=none")));
    assert!(!frozen_takes_badge_down(&frozen(&format!(
        "attention={ATTENTION_KEPT}"
    ))));
    let behind = format!("{ATTENTION_SKIPPED}{ESCALATED_BEHIND} (gui:9) answers this session");
    assert!(!frozen_takes_badge_down(&frozen(&behind)));
    let thawed = JournalRecord::of_line(1, Some("s-1"), "THAWED seq=5 input=clear", None);
    assert!(!frozen_takes_badge_down(&thawed));
    let bare = JournalRecord::of_line(
        1,
        Some("s-1"),
        &format!("{FROZEN} seq=4 input=stalled"),
        None,
    );
    assert!(!frozen_takes_badge_down(&bare));
}

#[test]
fn an_upgrade_row_tells_an_upgrade_from_a_relaunch() {
    let up = |t: i64, tab: &str, from: &str, to: &str, step: &str| {
        format!(
            "{{\"t\":{t},\"pid\":42,\"tab\":\"{tab}\",\"session\":\"c-1\",\"from\":\"{from}\",\
             \"to\":\"{to}\",\"step\":\"{step}\",\"detail\":\"cd '/x' && claude --resume c-1\"}}"
        )
    };
    let lines = [
        up(100, "s-1", "2.1.299", "2.1.300(managed)", "relaunched"),
        up(101, "s-2", "2.1.300", "2.1.300(same)", "relaunched"),
        up(102, "s-3", "-", "2.1.300(npm)", "relaunched"),
        up(103, "s-3", "2.1.300", "-", "relaunched"),
        up(104, "-", "2.1.299", "2.1.300(managed)", "exited"),
        up(105, "s-1", "2.1.299", "2.1.300(managed)", "adopted"),
        up(106, "s-1", "2.1.299", "2.1.300(managed)", "wait:busy"),
        up(107, "s-1", "2.1.300", "model:opus", "model-ride-dropped"),
    ];
    let fold = |since_ms| {
        let mut acc = HarnessStats::new(since_ms);
        for line in &lines {
            fold_upgrade_line(&mut acc, line);
        }
        acc
    };
    let acc = fold(0);
    assert_eq!((acc.upgraded, acc.relaunched), (1, 3), "{acc:#?}");
    assert_eq!((acc.rows, acc.sessions, acc.bad), (8, 3, 0));
    assert!(acc.by_rule.is_empty());
    let acc = fold(101_000);
    assert_eq!((acc.upgraded, acc.relaunched, acc.rows), (0, 3, 7));
    assert_eq!(fold(200_000).rows, 0);
}

#[test]
fn a_bad_line_is_counted_and_never_panics() {
    let good = typed(1_000, RULE_CONTINUE, "keep going");
    let half = &good[..good.len() / 2];
    let garbage = [
        "",
        "{",
        half,
        "[1,2,3]",
        "null",
        "\"a string\"",
        "{\"ts\":\"1000\",\"decision\":\"typed\"}",
        "{\"ts\":1.5e3}",
        "{\"t\":99999999999999999999999}",
        "{\"decision\":\"typed\"}",
        "\u{feff}\u{2028}é😀",
    ];
    let mut acc = HarnessStats::new(0);
    for line in garbage {
        fold_ledger_line(&mut acc, line);
        fold_upgrade_line(&mut acc, line);
    }
    fold_ledger_line(&mut acc, &good);
    assert_eq!(acc.bad, 2 * garbage.len() as u64, "{acc:#?}");
    assert_eq!((acc.continued, acc.rows), (1, 1));
    fold_journal(&mut acc, &[]);
    assert_eq!(acc.rows, 1);
    let acc = fold_ledger(std::slice::from_ref(&good), 0);
    assert_eq!((acc.bad, acc.rows), (0, 1));
    assert!(fold_ledger(&[good], 2_000).is_empty());
    assert!(HarnessStats::new(0).is_empty());
}

fn self_claimed(ts: i64, sid: &str, holder: &str, act: &str) -> String {
    approvals::SelfClaimRow { holder, act }.to_json(ts, Some(sid))
}

/// A PROVEN SELF-CLAIM'S ACTS ARE ITS HOLDER'S, NEVER ATERM'S: in a ledger beside aterm's own
/// rows they count under the holder's name (every process of it one name) and in the session's
/// share, each as aterm's own act of its rule, a landing and a re-exec booked apart, a point left
/// to a person an act that counts nothing; aterm's own counts, rows and sessions read exactly
/// what the ledger's old rows alone read, and an act before the window counts nowhere. Each
/// family holds a count no other family holds, so an act booked in another's slot changes what
/// the fold reads.
/// NEGATIVE CONTROLS: the old rows alone fold to no self-claim at all, a self-claim's row with no
/// holder is a bad line, and the same rules typed by aterm's own loop count as aterm's.
#[test]
fn a_self_claims_acts_count_as_its_holders_and_never_as_aterms() {
    let t = 10_000;
    let old = vec![
        said(t, RULE_READ_ONLY, Outcome::Approved, ""),
        typed(t, RULE_CONTINUE, "keep going"),
        said(t, "decline@v1", Outcome::Declined, "no spend"),
    ];
    let mut lines = old.clone();
    let mut ts = t;
    for (holder, act, times) in [
        ("codex-alab@4242", "approve-all@v1", 1),
        ("codex-alab@4242", "plan-approve@v2", 1),
        ("codex-alab@77", "answer-recommended@v1", 1),
        ("codex-alab@4242", "answer-long-term@v1", 1),
        ("codex-alab@4242", "answer-short-term@v1", 1),
        ("codex-alab@4242", "continue@v1", 1),
        ("codex-alab@4242", "api-retry@v1", 1),
        ("codex-alab@4242", "api-back@v1", 1),
        ("codex-alab@4242", "usage-resume@v1", 1),
        ("codex-alab@4242", "api-cutoff@v1", 1),
        ("codex-alab@4242", "land@v1", 3),
        ("codex-alab@4242", "land-nudge@v1", 2),
        ("codex-alab@4242", "landed@v1", 6),
        ("codex-alab@4242", "upgrade@v1", 7),
        ("codex-alab@4242", "left-to-person@v1", 1),
    ] {
        for _ in 0..times {
            lines.push(self_claimed(ts, "s-9", holder, act));
            ts += 1;
        }
    }
    lines.push(self_claimed(t - 1, "s-9", "codex-alab@4242", "continue@v1"));
    let fold = |lines: &[String]| {
        let mut acc = HarnessStats::per_session(t);
        for line in lines {
            fold_ledger_line(&mut acc, line);
        }
        acc.end_reading();
        acc
    };
    let (both, alone) = (fold(&lines), fold(&old));
    assert!(
        alone.self_claimed.is_empty(),
        "NEGATIVE CONTROL: {alone:#?}"
    );
    assert_eq!(counts(&both), counts(&alone), "aterm's own counts");
    assert_eq!(
        (
            both.rows,
            both.sessions,
            both.bad,
            &both.by_rule,
            both.landed
        ),
        (
            alone.rows,
            alone.sessions,
            alone.bad,
            &alone.by_rule,
            alone.landed
        )
    );
    assert_eq!((alone.rows, alone.sessions), (3, 1));
    let codex = SelfClaimed {
        approved: 2,
        answered: 3,
        continued: 1,
        resumed: 4,
        land_prompts: 5,
        landed: 6,
        upgraded: 7,
        acts: 29,
    };
    assert_eq!(
        both.self_claimed,
        BTreeMap::from([("codex-alab".to_string(), codex.clone())]),
        "one name for every process of it"
    );
    let shares = both.by_session.as_ref().expect("shares");
    assert_eq!(shares["s-9"].self_claimed["codex-alab"], codex);
    assert_eq!(counts_of(&shares["s-9"]), [0; 8], "none of aterm's own");
    assert!(shares["s-1"].self_claimed.is_empty());
    assert_eq!(
        alone.by_session.as_ref().map(|s| s.contains_key("s-9")),
        Some(false)
    );
    let mut held = HarnessStats::new(0);
    fold_ledger_line(&mut held, &self_claimed(t, "s-9", "", "continue@v1"));
    assert_eq!((held.bad, held.rows), (1, 0), "NEGATIVE CONTROL: no holder");
    assert!(held.self_claimed.is_empty());
    let typed_by_aterm = fold(&[
        typed(t, RULE_CONTINUE, "keep going"),
        typed(t, "land@v1", "[aterm harness] Autopilot is set to Land …"),
    ]);
    assert_eq!(
        (typed_by_aterm.continued, typed_by_aterm.land_prompts),
        (1, 1),
        "NEGATIVE CONTROL: aterm's own rows of the rules count as aterm's"
    );
    assert!(typed_by_aterm.self_claimed.is_empty());
}

fn counts_of(t: &Tally) -> [u64; 8] {
    [
        t.approved,
        t.declined,
        t.answered,
        t.continued,
        t.land_prompts,
        t.landed,
        t.resumed,
        t.upgraded,
    ]
}

#[test]
fn a_window_is_cut_only_where_a_long_file_begins_inside_it() {
    let keep = approvals::KEEP_ROWS;
    assert!(window_cut(keep, keep, Some(1_001), 1_000));
    assert!(window_cut(keep + 7, keep, Some(1_001), 1_000));
    let ring = window_cut(READ_MAX_ROWS, usize::MAX, Some(1_001), 1_000);
    assert!(ring, "this read's own ring cuts too");
    assert!(!window_cut(keep - 1, keep, Some(1_001), 1_000));
    assert!(!window_cut(keep, keep, Some(1_000), 1_000));
    assert!(!window_cut(keep, keep, Some(999), 1_000));
    assert!(!window_cut(keep, keep, None, 1_000));
}

fn listing(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut todo = vec![root.to_path_buf()];
    while let Some(dir) = todo.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.filter_map(Result::ok) {
            let path = e.path();
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                todo.push(path.clone());
            }
            out.push(path);
        }
    }
    out.sort();
    out
}

fn sweep_opts(home: &Path, harness_state: &Path) -> Opts {
    Opts {
        home: home.to_path_buf(),
        state: harness_state.to_path_buf(),
        sock: None,
        token: None,
        only_sid: None,
        dry_run: false,
        human_grace_s: 120,
        hand_back: true,
        background: false,
        aterm_state: None,
        ask: false,
        several_logins: false,
        usage_credits: false,
        usage_tap: None,
        instances: Instances::Listed(Vec::new()),
        agents_off: Vec::new(),
        land: false,
        tell: crate::harness::upgrade_drive::Tell::Off,
        press: None,
        timings: crate::harness::Timings::FAST,
    }
}

fn report(tab: &str, from: &str, to: &str, step: &str) -> Report {
    Report {
        pid: 42,
        tab: tab.to_string(),
        session: "c-1".to_string(),
        from: from.to_string(),
        to: to.to_string(),
        step: step.to_string(),
    }
}

#[test]
fn stats_under_reads_every_sessions_files_and_creates_nothing() {
    const NOW_S: u64 = 1_790_311_076;
    let at = i64::try_from(NOW_S).expect("seconds") * 1000;
    let root = ScratchDir::new("stats", "under");
    let state = root.join("state");
    let harness = root.join("harness");
    let drive = state.join("drive");
    std::fs::create_dir_all(&drive).expect("drive");
    let write = |name: &str, lines: &[String]| {
        let mut body = lines.join("\n");
        body.push('\n');
        std::fs::write(drive.join(name), body).expect("write");
    };
    write(
        "s-1.jsonl",
        &[
            said(at, RULE_READ_ONLY, Outcome::Approved, ""),
            typed(at + 1, RULE_CONTINUE, "keep going"),
            said(at + 2, RULE_WALL_WAIT, Outcome::Waiting, "(wall: …)"),
            typed(at + 9_002, RULE_LIMIT_RESUME, "keep going"),
        ],
    );
    let account = "relaunch under account w-2";
    write(
        "s-2.jsonl",
        &[
            row(at + 10, "s-2", "decline@v1", Outcome::Declined, "Buy?", "-"),
            row(at + 11, "s-2", RULE_ACCOUNT, Outcome::Typed, account, "-"),
        ],
    );
    let journal_1 = journal(&format!(
        "{} s-1 ESCALATED seq=10 attention=OK mail=OK 7 off=91
         {} s-1 CLEARED seq=14 box attention=OK
         {} s-1 CHOSE seq=40 rule=answer-recommended@v1 policy=recommended Q → A",
        at + 20,
        at + 5_020,
        at + 5_030
    ));
    let mut lines: Vec<String> = journal_1.iter().map(JournalRecord::to_json).collect();
    lines.insert(1, "{\"t\":".to_string());
    write("s-1.journal.jsonl", &lines);
    let decoy = [row(at, "s-9", RULE_READ_ONLY, Outcome::Approved, "ls", "")];
    write("s-1.goal-hold.json", &decoy);
    write("s-1.jsonl.tmp", &decoy);
    std::fs::create_dir_all(drive.join("d.jsonl")).expect("a directory");
    let opts = sweep_opts(&root.join("home"), &harness);
    upgrade_drive::at_second(NOW_S, || {
        let moved = report("s-1", "2.1.299", "2.1.300(managed)", "relaunched");
        upgrade_drive::ledger(&opts, &moved, "cd '/x'");
        let crashed = report("s-3", "2.1.300", "2.1.300(same)", "relaunched");
        upgrade_drive::ledger(&opts, &crashed, "cd '/x'");
        let exited = report("s-3", "2.1.300", "2.1.300(same)", "exited");
        upgrade_drive::ledger(&opts, &exited, "");
    });
    let path = upgrade_ledger(&harness);
    assert_eq!(upgrade_drive::ledger_path(&opts), path, "the writer's file");

    let before = listing(&root);
    let acc = stats_under(&state, &harness, at);
    assert_eq!(listing(&root), before, "the read created nothing");
    assert_eq!(acc.since_ms, at);
    assert_eq!(counts(&acc), [1, 0, 1, 1, 1, 0, 1, 1, 1, 1], "{acc:#?}");
    assert_eq!((acc.human_wait_ms, acc.limit_wait_ms), (5_000, 9_000));
    assert_eq!(acc.sessions, 3, "s-1, s-2 and the upgrade's s-3");
    assert_eq!((acc.rows, acc.bad, acc.cut), (6 + 3 + 3, 1, false));
    assert_eq!(by(&acc, RULE_ACCOUNT), 1);
    assert_eq!(by(&acc, RULE_READ_ONLY), 1, "the decoys were not read");

    let later = stats_under(&state, &harness, at + 3_600_000);
    assert!(later.is_empty(), "{later:#?}");
    assert_eq!(listing(&root), before);
}

#[test]
fn a_missing_state_dir_is_empty_and_stays_missing() {
    let root = ScratchDir::new("stats", "missing");
    let state = root.join("state");
    let harness = root.join("harness");
    let acc = stats_under(&state, &harness, 0);
    assert!(acc.is_empty());
    assert_eq!((acc.rows, acc.bad, acc.sessions, acc.cut), (0, 0, 0, false));
    assert!(!state.exists() && !harness.exists(), "nothing was made");
    assert!(listing(&root).is_empty());
    std::fs::create_dir_all(state.join("drive")).expect("drive");
    assert!(stats_under(&state, &harness, 0).is_empty());
    assert!(!harness.exists());
}

#[test]
fn a_ledger_cut_inside_the_window_says_so() {
    let root = ScratchDir::new("stats", "cut");
    let state = root.join("state");
    let harness = root.join("harness");
    std::fs::create_dir_all(state.join("drive")).expect("drive");
    let path = approvals::ledger_under(&state, Some("s-1"));
    let rows = |n: usize| {
        let mut body = String::new();
        for k in 0..n {
            let ts = 10_000 + i64::try_from(k).expect("small");
            body.push_str(&typed(ts, RULE_CONTINUE, "keep going"));
            body.push('\n');
        }
        body
    };
    std::fs::write(&path, rows(approvals::KEEP_ROWS)).expect("ledger");
    let acc = stats_under(&state, &harness, 5_000);
    assert!(acc.cut, "1024 rows, the oldest after the window opened");
    assert_eq!(acc.continued, approvals::KEEP_ROWS as u64);
    assert!(!stats_under(&state, &harness, 10_000).cut);
    std::fs::write(&path, rows(approvals::KEEP_ROWS - 1)).expect("ledger");
    assert!(!stats_under(&state, &harness, 5_000).cut);
}

#[test]
fn a_ledger_untouched_since_the_window_is_not_read() {
    let root = ScratchDir::new("stats", "mtime");
    let state = root.join("state");
    let harness = root.join("harness");
    std::fs::create_dir_all(state.join("drive")).expect("drive");
    let path = approvals::ledger_under(&state, Some("s-1"));
    let since_ms: i64 = 1_790_000_000_000;
    let line = typed(since_ms + 1, RULE_CONTINUE, "keep going");
    std::fs::write(&path, line + "\n").expect("ledger");
    let set_mtime = |at: std::time::SystemTime| set_modified(&path, at);
    let day_before = u64::try_from(since_ms - 86_400_000).expect("after the epoch");
    set_mtime(std::time::UNIX_EPOCH + std::time::Duration::from_millis(day_before));
    assert!(stats_under(&state, &harness, since_ms).is_empty());
    set_mtime(std::time::SystemTime::now());
    assert_eq!(stats_under(&state, &harness, since_ms).continued, 1);
}

type Recorded = Vec<(String, &'static str, Outcome, &'static str, &'static str)>;

fn the_recorded_session() -> (Recorded, &'static str) {
    let t = 1_792_249_200_000;
    let rows = vec![
        (RULE_ALLOW_ONCE, Outcome::Approved, "cargo test", ""),
        (
            RULE_READ_ONLY,
            Outcome::Approved,
            "git log",
            "unproven: not read-only: touch",
        ),
        (RULE_ANSWER_RECOMMENDED, Outcome::Approved, "", "tab"),
        (RULE_ANSWER_RECOMMENDED, Outcome::Approved, "", "toggle"),
        (RULE_ANSWER_RECOMMENDED, Outcome::Approved, "", "submit"),
        (RULE_ANSWER, Outcome::Typed, "Decide for yourself.", ""),
        (RULE_CONTINUE, Outcome::Typed, "keep going", ""),
        ("context-compact@v1", Outcome::Typed, "/compact", ""),
        (
            "context-compact@v1",
            Outcome::Typed,
            "Carry on where you left off.",
            "",
        ),
        (
            "land@v1",
            Outcome::Typed,
            "[aterm harness] Autopilot is set to Land",
            "",
        ),
        (
            "land-nudge@v1",
            Outcome::Typed,
            "[aterm harness] Still landing",
            "",
        ),
        ("decline@v1", Outcome::Declined, "rm -rf /", "no spend"),
        (RULE_LOGIN, Outcome::Typed, "/login", ""),
        (RULE_READ_ONLY, Outcome::Skipped, "ls", "moved"),
        ("-", Outcome::Escalated, "", "no rule answers this box"),
    ];
    let rows = rows
        .into_iter()
        .enumerate()
        .map(|(i, (rule, outcome, command, reason))| {
            let ts = t + i64::try_from(i).expect("few rows") * 100;
            (
                row(ts, "s-1", rule, outcome, command, reason),
                rule,
                outcome,
                command,
                reason,
            )
        })
        .collect();
    let journal = "
        1792249200250 s-1 CHOSE seq=40 rule=answer-recommended@v1 policy=recommended Which colour? → Blue
        1792249200260 s-1 TYPED seq=44 rule=context-compact@v1 /compact
    ";
    (rows, journal)
}

#[test]
fn the_sort_counts_the_recorded_acts_without_counting_dialog_keys() {
    let (rows, fixture) = the_recorded_session();
    let recs = journal(fixture);
    let since = 1_792_249_000_000;
    let mut after = HarnessStats::new(since);
    for (line, ..) in &rows {
        fold_ledger_line(&mut after, line);
    }
    fold_journal(&mut after, &recs);
    assert_eq!(counts(&after), [2, 1, 1, 2, 1, 2, 1, 0, 0, 0], "{after:#?}");
    assert_eq!(after.rows, 17, "15 ledger rows and two journal entries");
    assert_eq!(after.sessions, 1);
    let expected = [
        (RULE_ALLOW_ONCE, 1),
        (RULE_READ_ONLY, 1),
        (RULE_ANSWER_RECOMMENDED, 3),
        (RULE_ANSWER, 1),
        (RULE_CONTINUE, 1),
        ("context-compact@v1", 2),
        ("land@v1", 1),
        ("land-nudge@v1", 1),
        ("decline@v1", 1),
        (RULE_LOGIN, 1),
    ];
    assert_eq!(
        after.by_rule,
        expected
            .into_iter()
            .map(|(rule, n)| (rule.to_string(), n))
            .collect()
    );
    let mut by_act = HarnessStats::new(since);
    for (_, rule, outcome, command, _) in &rows {
        let act = crate::harness::acts::sort(rule, *outcome, command).act;
        let act = act.or_else(|| {
            (*rule == "context-compact@v1").then_some(crate::harness::acts::Act::Resumed)
        });
        match act {
            Some(crate::harness::acts::Act::Answered) => by_act.answered += 1,
            Some(crate::harness::acts::Act::Resumed) => by_act.resumed += 1,
            _ => {}
        }
    }
    fold_journal(&mut by_act, &recs);
    assert_eq!(
        (by_act.answered, by_act.resumed),
        (5, 2),
        "CONTROL: three keys, a CHOSE and a prose answer; /compact and its continuation"
    );
    assert_ne!(
        (by_act.answered, by_act.resumed),
        (after.answered, after.resumed)
    );
}

fn now_ms() -> i64 {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after the epoch");
    i64::try_from(since.as_millis()).expect("ms")
}

fn one_pass_tree(today: i64, week: i64) -> (ScratchDir, PathBuf, PathBuf, Vec<PathBuf>) {
    let root = ScratchDir::new("stats", "one-pass");
    let state = root.join("state");
    let harness = root.join("harness");
    let drive = state.join("drive");
    std::fs::create_dir_all(&drive).expect("drive");
    std::fs::create_dir_all(harness.join("upgrade")).expect("upgrade");
    let write = |path: &Path, lines: &[String], modified: Option<i64>| {
        let mut body = lines.join("\n");
        body.push('\n');
        std::fs::write(path, body).expect("write");
        if let Some(ms) = modified {
            let at = std::time::UNIX_EPOCH
                + std::time::Duration::from_millis(u64::try_from(ms).expect("after the epoch"));
            set_modified(path, at);
        }
    };
    let hour = 3_600_000;
    let (rows, recorded_journal) = the_recorded_session();
    let recorded_t0 = 1_792_249_200_000;
    let shift = today + hour - recorded_t0;
    let mut s1: Vec<String> = vec![
        said(today - 2 * hour, RULE_READ_ONLY, Outcome::Approved, ""),
        said(
            today - 30 * 60_000,
            RULE_WALL_WAIT,
            Outcome::Waiting,
            "(wall: …)",
        ),
        typed(today + 20 * 60_000, RULE_LIMIT_RESUME, "keep going"),
    ];
    s1.extend(
        rows.iter()
            .enumerate()
            .map(|(i, (_, rule, outcome, command, reason))| {
                let ts = recorded_t0 + shift + i64::try_from(i).expect("few") * 100;
                row(ts, "s-1", rule, *outcome, command, reason)
            }),
    );
    let s1_ledger = drive.join("s-1.jsonl");
    write(&s1_ledger, &s1, None);
    let mut s1_journal = journal(&format!(
        "{} s-1 ESCALATED seq=10 attention=OK mail=OK 7 off=91
         {} s-1 CLEARED seq=14 box attention=OK
         {} s-1 ESCALATED seq=20 attention=OK mail=OK 8 off=92
         {} s-1 CLEARED seq=24 box attention=OK",
        today - hour,
        today + 10 * 60_000,
        today + 2 * hour,
        today + 2 * hour + 120_000,
    ));
    s1_journal.extend(journal(recorded_journal).into_iter().map(|mut r| {
        r.t += shift;
        r
    }));
    let landed_at = today - 9 * hour;
    s1_journal.extend(journal(&format!(
        "{} s-1 CONTINUED seq=90 rule={RULE_LAND} land it",
        landed_at - hour
    )));
    s1_journal.push(land_note(landed_at, "s-1", DEFENDED, None));
    s1_journal.push(land_note(
        today + 30 * 60_000,
        "s-1",
        DEFENDED,
        Some(landed_at),
    ));
    s1_journal.extend(journal(&format!(
        "{} s-1 CONTINUED seq=300 rule={RULE_LAND_NUDGE} still landing",
        today + 3 * hour
    )));
    s1_journal.push(land_note(today + 3 * hour + 60_000, "s-1", DEFENDED, None));
    let mut lines: Vec<String> = s1_journal.iter().map(JournalRecord::to_json).collect();
    lines.insert(2, "{\"t\":".to_string());
    let s1_journal = drive.join("s-1.journal.jsonl");
    write(&s1_journal, &lines, None);
    let yesterday = today - 10 * hour;
    let s2_ledger = drive.join("s-2.jsonl");
    write(
        &s2_ledger,
        &[
            row(
                yesterday - 5,
                "s-2",
                "decline@v1",
                Outcome::Declined,
                "Buy?",
                "-",
            ),
            "not a row".to_string(),
            row(
                yesterday - 4,
                "s-2",
                RULE_CONTINUE,
                Outcome::Typed,
                "go on",
                "-",
            ),
        ],
        Some(yesterday),
    );
    let s2_journal = drive.join("s-2.journal.jsonl");
    let mut s2_lines: Vec<String> = journal(&format!(
        "{} s-2 ESCALATED seq=3 attention=OK mail=OK 1 off=1",
        yesterday - 3
    ))
    .iter()
    .map(JournalRecord::to_json)
    .collect();
    s2_lines.push("{\"t\":".to_string());
    write(&s2_journal, &s2_lines, Some(yesterday));
    let s3_ledger = drive.join("s-3.jsonl");
    let s3: Vec<String> = (0..approvals::KEEP_ROWS)
        .map(|k| {
            let ts = today + 3 * hour + i64::try_from(k).expect("small");
            row(ts, "s-3", RULE_CONTINUE, Outcome::Typed, "keep going", "-")
        })
        .collect();
    write(&s3_ledger, &s3, None);
    let s4_ledger = drive.join("s-4.jsonl");
    let old = week - 2 * 86_400_000;
    write(
        &s4_ledger,
        &[row(old, "s-4", RULE_CONTINUE, Outcome::Typed, "go", "-")],
        Some(old),
    );
    let up = |t: i64, tab: &str, from: &str, to: &str, step: &str| {
        format!(
            "{{\"t\":{},\"pid\":42,\"tab\":\"{tab}\",\"session\":\"c-1\",\"from\":\"{from}\",\
             \"to\":\"{to}\",\"step\":\"{step}\",\"detail\":\"-\"}}",
            t / 1000
        )
    };
    let upgrades = upgrade_ledger(&harness);
    write(
        &upgrades,
        &[
            up(yesterday, "s-2", "2.1.300", "2.1.300(same)", "relaunched"),
            "{\"t\":".to_string(),
            up(
                today + hour,
                "s-1",
                "2.1.299",
                "2.1.300(managed)",
                "relaunched",
            ),
            up(today + hour, "-", "2.1.300", "2.1.300(same)", "relaunched"),
        ],
        None,
    );
    let files = vec![
        s1_ledger, s1_journal, s2_ledger, s2_journal, s3_ledger, s4_ledger, upgrades,
    ];
    (root, state, harness, files)
}

fn without_shares(acc: &HarnessStats) -> HarnessStats {
    HarnessStats {
        by_session: None,
        decided: None,
        ..acc.clone()
    }
}

#[test]
fn one_pass_folds_both_windows_as_two_readings_do() {
    let now = now_ms();
    let today = now - 5 * 3_600_000;
    let week = now - 7 * 86_400_000;
    let (_root, state, harness, _) = one_pass_tree(today, week);
    let (one_today, one_week) = stats_windows(&state, &harness, today, week);
    let two_today = stats_under(&state, &harness, today);
    let two_week = stats_under(&state, &harness, week);
    assert_eq!(without_shares(&one_today), two_today);
    assert_eq!(one_week, two_week, "the week keeps no shares");
    assert!(one_week.by_session.is_none() && one_today.by_session.is_some());
    let continued = approvals::KEEP_ROWS as u64 + 1;
    assert_eq!(
        counts(&two_today),
        [2, 1, 1, 2, continued, 2, 2, 1, 1, 1],
        "{two_today:#?}"
    );
    assert_eq!(
        counts(&two_week),
        [3, 1, 2, 2, continued + 1, 2, 2, 3, 2, 1],
        "{two_week:#?}"
    );
    assert_eq!(
        (two_today.human_wait_ms, two_today.limit_wait_ms),
        (10 * 60_000 + 120_000, 20 * 60_000)
    );
    assert_eq!(
        (two_week.human_wait_ms, two_week.limit_wait_ms),
        (70 * 60_000 + 120_000, 50 * 60_000)
    );
    assert_eq!(
        (two_today.bad, two_week.bad),
        (2, 4),
        "s-2's bad lines, its ledger's and its journal's, are the week's"
    );
    assert!(two_today.cut && two_week.cut, "s-3 began inside both");
    assert_eq!((two_today.sessions, two_week.sessions), (2, 3));
    assert_eq!(
        (two_today.landed, two_week.landed),
        (1, 2),
        "s-1's landings: today's, and yesterday's restated today"
    );
    let mut forced = HarnessStats::new(today);
    for line in tail_lines(&state.join("drive").join("s-2.jsonl"), READ_MAX_ROWS).expect("s-2") {
        fold_ledger_line(&mut forced, &line);
    }
    assert_eq!(
        forced.bad, 1,
        "CONTROL: an untouched file folded is not left unread"
    );
}

#[test]
fn a_reading_reads_each_file_once() {
    let now = now_ms();
    let today = now - 5 * 3_600_000;
    let week = now - 7 * 86_400_000;
    let (_root, state, harness, files) = one_pass_tree(today, week);
    let mut reads: BTreeMap<PathBuf, u32> = BTreeMap::new();
    let mut counting = |path: &Path| {
        *reads.entry(path.to_path_buf()).or_insert(0) += 1;
        tail_lines(path, READ_MAX_ROWS)
    };
    let (one_today, one_week) = stats_windows_with(&state, &harness, today, week, &mut counting);
    let s4 = state.join("drive").join("s-4.jsonl");
    let expected: BTreeMap<PathBuf, u32> = files
        .iter()
        .filter(|path| **path != s4)
        .map(|path| (path.clone(), 1))
        .collect();
    assert_eq!(reads, expected, "each file once; s-4 not at all");
    assert_eq!(
        (one_today.clone(), one_week),
        stats_windows(&state, &harness, today, week),
        "the seam reads what tail_lines reads"
    );
    let decided = one_today.decided.as_ref().expect("today keeps decisions");
    let kinds: Vec<DecisionKind> = decided.rows.iter().map(|d| d.kind).collect();
    assert_eq!(
        kinds,
        [
            DecisionKind::Unproven,
            DecisionKind::Chose,
            DecisionKind::Answered,
            DecisionKind::Declined
        ],
        "{decided:#?}"
    );
    let mut twice: BTreeMap<PathBuf, u32> = BTreeMap::new();
    for since in [today, week] {
        let mut counting = |path: &Path| {
            *twice.entry(path.to_path_buf()).or_insert(0) += 1;
            tail_lines(path, READ_MAX_ROWS)
        };
        let _ = stats_windows_with(&state, &harness, since, since, &mut counting);
    }
    assert_eq!(twice[&files[0]], 2, "CONTROL: two passes read s-1 twice");
}

#[test]
fn todays_shares_sum_to_its_totals_and_a_cut_marks_its_own_session() {
    let now = now_ms();
    let today = now - 5 * 3_600_000;
    let week = now - 7 * 86_400_000;
    let (_root, state, harness, _) = one_pass_tree(today, week);
    let (acc, _) = stats_windows(&state, &harness, today, week);
    let shares = acc.by_session.clone().expect("today keeps shares");
    let sum = shares.values().fold(Tally::default(), |mut sum, t| {
        sum.approved += t.approved;
        sum.unproven += t.unproven;
        sum.declined += t.declined;
        sum.answered += t.answered;
        sum.continued += t.continued;
        sum.land_prompts += t.land_prompts;
        sum.landed += t.landed;
        sum.resumed += t.resumed;
        sum.escalated += t.escalated;
        sum.needs_told += t.needs_told;
        sum.human_wait_ms += t.human_wait_ms;
        sum.limit_wait_ms += t.limit_wait_ms;
        sum.relaunched += t.relaunched;
        sum.upgraded += t.upgraded;
        sum
    });
    let totals = Tally {
        approved: acc.approved,
        unproven: acc.unproven,
        declined: acc.declined,
        answered: acc.answered,
        continued: acc.continued,
        land_prompts: acc.land_prompts,
        landed: acc.landed,
        resumed: acc.resumed,
        escalated: acc.escalated,
        needs_told: acc.needs_told,
        human_wait_ms: acc.human_wait_ms,
        limit_wait_ms: acc.limit_wait_ms,
        relaunched: acc.relaunched,
        upgraded: acc.upgraded,
        last_escalated_ms: None,
        self_claimed: BTreeMap::new(),
        cut: false,
    };
    assert_eq!(sum, totals, "{shares:#?}");
    let s1 = &shares["s-1"];
    assert_eq!(
        (
            s1.approved,
            s1.unproven,
            s1.answered,
            s1.land_prompts,
            s1.upgraded
        ),
        (2, 1, 2, 2, 1)
    );
    assert_eq!(s1.last_escalated_ms, Some(today + 2 * 3_600_000));
    assert_eq!((s1.landed, acc.landed), (1, 1), "today's landing is s-1's");
    assert!(!s1.cut, "s-1 is whole");
    assert!(shares["s-3"].cut, "the cut file's own session");
    assert_eq!(shares["s-3"].continued, approvals::KEEP_ROWS as u64);
    assert_eq!(shares["-"].relaunched, 1, "the tab-less upgrade row");
    assert!(!shares.contains_key("s-2"), "untouched today: no share");
    assert!(HarnessStats::new(today).by_session.is_none());
}

fn decided_tree(files: &[(&str, Vec<String>)]) -> (ScratchDir, PathBuf, PathBuf) {
    let root = ScratchDir::new("stats", "decided");
    let state = root.join("state");
    let harness = root.join("harness");
    let drive = state.join("drive");
    std::fs::create_dir_all(&drive).expect("drive");
    for (name, lines) in files {
        let mut body = lines.join("\n");
        body.push('\n');
        std::fs::write(drive.join(name), body).expect("write");
    }
    (root, state, harness)
}

fn seq_row(
    ts: i64,
    sid: &str,
    rule_id: &str,
    outcome: Outcome,
    (command, reason): (&str, &str),
    box_seq: u64,
) -> String {
    Row {
        rule_id,
        outcome,
        command,
        reason,
        box_seq,
    }
    .to_json(ts, Some(sid))
}

fn journal_lines(fixture: &str) -> Vec<String> {
    journal(fixture)
        .iter()
        .map(JournalRecord::to_json)
        .collect()
}

fn decided_day(t: i64) -> (Vec<String>, Vec<String>) {
    let journal = journal_lines(&format!(
        "{} s-1 CHOSE seq=57 rule=answer-recommended@v1 policy=recommended Which colour? \u{2192} Blue (Recommended) | Which size? \u{2192} Large
         {} s-1 EVENT question seq=60 \u{23fa} Should I add a CLI or packaging next?
         {} s-1 EVENT idle seq=64 - Done for now.",
        t + 9_000,
        t + 20_000,
        t + 21_000,
    ));
    let key = |ts, command, reason, seq| {
        seq_row(
            ts,
            "s-1",
            RULE_ANSWER_RECOMMENDED,
            Outcome::Approved,
            (command, reason),
            seq,
        )
    };
    let heredoc = "python3 - <<PY\nprint(1)\nPY";
    let press = |ts| {
        seq_row(
            ts,
            "s-1",
            RULE_READ_ONLY,
            Outcome::Approved,
            (heredoc, "unproven: not read-only: python3"),
            70,
        )
    };
    let ledger = vec![
        key(
            t + 1_000,
            "Which colour? \u{2192} Blue (Recommended)",
            "unproven: answered with its recommended option",
            50,
        ),
        key(
            t + 2_000,
            "Which size? \u{2192} Large",
            "unproven: no option is marked (Recommended): option 1",
            52,
        ),
        key(
            t + 3_000,
            "submit answers (2 of 2 answered)",
            "unproven: submitted the answers its review lists",
            54,
        ),
        seq_row(
            t + 20_500,
            "s-1",
            RULE_LAND_ANSWER,
            Outcome::Typed,
            (LAND_ANSWER, "the turn-end policy"),
            61,
        ),
        press(t + 30_000),
        press(t + 50_000),
        press(t + 100_000),
        seq_row(
            t + 200_000,
            "s-1",
            "decline@v1",
            Outcome::Declined,
            ("rm -rf build", "Nothing outside the worktree is removed."),
            80,
        ),
    ];
    (journal, ledger)
}

type Seen<'a> = (
    DecisionKind,
    i64,
    &'a str,
    bool,
    &'a str,
    Option<&'a str>,
    u32,
);

#[test]
fn the_join_lists_each_choice_made_in_the_persons_name() {
    let t = now_ms() - 3_600_000;
    let (told, ledger) = decided_day(t);
    let (_root, state, harness) =
        decided_tree(&[("s-1.journal.jsonl", told), ("s-1.jsonl", ledger.clone())]);
    let (today, week) = stats_windows(&state, &harness, t - 60_000, t - 86_400_000);
    assert!(week.decided.is_none(), "the week keeps none");
    let decided = today.decided.expect("today keeps decisions");
    assert_eq!(decided.total, 4);
    let rows: Vec<Seen<'_>> = decided
        .rows
        .iter()
        .map(|d| {
            (
                d.kind,
                d.t_ms - t,
                &*d.asked,
                d.said,
                &*d.did,
                d.why.as_deref(),
                d.times,
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (
                DecisionKind::Chose,
                2_000,
                "Which colour? / Which size?",
                false,
                "Blue; Large",
                Some("the option marked Recommended where one was, else the first"),
                1
            ),
            (
                DecisionKind::Answered,
                20_500,
                "Should I add a CLI or packaging next?",
                false,
                "the land answer: land all of the work on the remote main soonest, start nothing new",
                None,
                1
            ),
            (
                DecisionKind::Unproven,
                30_000,
                "python3 - <<PY \u{2026}",
                false,
                "",
                Some("Approve permission boxes: All"),
                3
            ),
            (
                DecisionKind::Declined,
                200_000,
                "rm -rf build",
                false,
                "",
                Some("Nothing outside the worktree is removed."),
                1
            ),
        ],
        "{:#?}",
        decided.rows
    );
    assert!(decided.rows.iter().all(|d| d.sid == "s-1"));
    let chose = journal(&format!(
        "{} s-1 CHOSE seq=57 rule=answer-recommended@v1 policy=recommended x",
        t
    ));
    let seqs: Vec<u64> = ledger
        .iter()
        .filter_map(|l| aterm_json::from_str::<Value>(l).ok())
        .filter_map(|v| v.get("box_seq").and_then(Value::as_u64))
        .collect();
    assert!(
        !seqs.contains(&chose[0].seq.expect("a seq")),
        "CONTROL: a join by box_seq finds no key: {seqs:?}"
    );
    assert_eq!(
        chose_why(&[], "recommended"),
        "Question priority is Recommended",
        "CONTROL: with no key, the why is the policy's word"
    );
    let passed = format!(
        "unproven: {CHOSE_FIRST_SAFE}: option 1 would reset, so option 2, the first that names \
         no destructive act and keeps no answer"
    );
    let first = format!("unproven: {CHOSE_UNMARKED}");
    let marked = format!("unproven: {CHOSE_RECOMMENDED}");
    assert_eq!(
        chose_why(&[passed.as_str()], "recommended"),
        "no option was marked Recommended, so it took the first that names no destructive act",
        "a passed-over option 1 is said, never read as the policy's word"
    );
    assert_eq!(
        chose_why(&[first.as_str(), passed.as_str()], "recommended"),
        "no option was marked Recommended, so it took the first that names no destructive act"
    );
    assert_eq!(
        chose_why(&[marked.as_str(), passed.as_str()], "recommended"),
        "the option marked Recommended where one was, else the first that names no destructive \
         act"
    );
    let mut plain = HarnessStats::new(t - 60_000);
    for line in &ledger {
        fold_ledger_line(&mut plain, line);
    }
    plain.end_reading();
    assert!(plain.decided.is_none());
}

fn chose_words(answers: &[String]) -> String {
    answers.join(" | ").chars().take(LINE_CHARS).collect()
}

#[test]
fn a_long_dialog_is_worded_from_its_keys() {
    let t = now_ms() - 3_600_000;
    let q1 = "Which colour should the footer of the settings page use on a dark theme?";
    let q2 = "Which side of the footer should the session chips go on, then?";
    let q3 = "Should the chips show the model's name as well as its effort level?";
    let qb = "Which colour should the footer of the settings page use on a dark theme, by default?";
    let tab = |q: &str, c: &str| format!("{q} \u{2192} {c}");
    let three = [
        tab(q1, "Blue (Recommended)"),
        tab(q2, "Right side, beside the facts (Recommended)"),
        tab(q3, "No"),
    ];
    let two = [
        tab(qb, "Blue"),
        tab(q2, "Right side, beside the facts (Recommended)"),
    ];
    let (cut3, cut2) = (chose_words(&three), chose_words(&two));
    let marked = "unproven: answered with its recommended option";
    let unmarked = "unproven: no option is marked (Recommended): option 1";
    let key = |ts, sid: &str, command: &str, reason: &str| {
        seq_row(
            ts,
            sid,
            RULE_ANSWER_RECOMMENDED,
            Outcome::Approved,
            (command, reason),
            1,
        )
    };
    let told = |sid: &str, at: i64, words: &str| {
        journal_lines(&format!(
            "{at} {sid} CHOSE seq=900 rule=answer-recommended@v1 policy=recommended {words}"
        ))
    };
    let files = [
        ("s-a.journal.jsonl", told("s-a", t + 9_000, &cut3)),
        (
            "s-a.jsonl",
            vec![
                key(t + 1_000, "s-a", &three[0], marked),
                key(t + 2_000, "s-a", &three[1], marked),
                key(t + 3_000, "s-a", &three[2], unmarked),
            ],
        ),
        ("s-b.journal.jsonl", told("s-b", t + 9_000, &cut2)),
        (
            "s-b.jsonl",
            vec![
                key(t + 1_000, "s-b", &two[0], unmarked),
                key(t + 2_000, "s-b", &two[1], marked),
            ],
        ),
        ("s-c.journal.jsonl", told("s-c", t + 9_000, &cut2)),
        ("s-c.jsonl", vec![key(t + 1_000, "s-c", &two[0], unmarked)]),
        (
            "s-d.journal.jsonl",
            journal_lines(&format!(
                "{} s-d CHOSE seq=950 rule=answer-long-term@v1 policy=long-term Ship it? \u{2192} the long-term policy text
                 {} s-d CHOSE seq=990 rule=answer-recommended@v1 policy=recommended Short? \u{2192} Yes",
                t + 40_000,
                t + 70_000,
            )),
        ),
        (
            "s-d.jsonl",
            vec![
                key(t + 30_000, "s-d", "Taken over? \u{2192} Theirs", unmarked),
                key(t + 60_000, "s-d", "Short? \u{2192} Yes, and more", marked),
                key(t + 80_000, "s-d", "Untold? \u{2192} Maybe", marked),
            ],
        ),
    ];
    let (_root, state, harness) = decided_tree(&files);
    let (today, _) = stats_windows(&state, &harness, t - 60_000, t - 86_400_000);
    let rows = today.decided.expect("decisions").rows;
    let of = |sid: &str| -> Vec<(i64, String, String, Option<String>)> {
        rows.iter()
            .filter(|d| d.sid == sid)
            .map(|d| {
                (
                    d.t_ms - t,
                    d.asked.to_string(),
                    d.did.to_string(),
                    d.why.as_deref().map(str::to_string),
                )
            })
            .collect()
    };
    let words = |text: &str| agent_words(text).to_string();
    assert_eq!(
        of("s-a"),
        [(
            3_000,
            words(&format!("{q1} / {q2} / {q3}")),
            "Blue; Right side, beside the facts; No".to_string(),
            Some("the option marked Recommended where one was, else the first".to_string())
        )]
    );
    assert_eq!(
        of("s-b"),
        [(
            2_000,
            words(&format!("{qb} / {q2}")),
            "Blue; Right side, beside the facts".to_string(),
            Some("the option marked Recommended where one was, else the first".to_string())
        )]
    );
    assert_eq!(
        of("s-c"),
        [(
            1_000,
            words(&format!("{qb} / {q2}")),
            "Blue; R\u{2026}".to_string(),
            Some("no option was marked Recommended, so it took the first".to_string())
        )]
    );
    assert_eq!(
        of("s-d"),
        [
            (
                40_000,
                "Ship it?".to_string(),
                "the long-term policy text".to_string(),
                Some("Question priority is Long-term".to_string())
            ),
            (
                70_000,
                "Short?".to_string(),
                "Yes".to_string(),
                Some("Question priority is Recommended".to_string())
            ),
        ],
        "a person's key and a key that only begins with a whole tab are no tab's"
    );
    assert_eq!(cut3.chars().count(), LINE_CHARS);
    assert!(cut3.ends_with(&format!("{q2} \u{2192}")), "{cut3}");
    assert!(cut2.ends_with(&format!("{q2} \u{2192} R")), "{cut2}");
    assert!("Short? \u{2192} Yes".chars().count() < LINE_CHARS);
}

#[test]
fn a_run_of_unproven_presses_is_one_row_by_its_head() {
    let t = now_ms() - 3_600_000;
    let press = |ts: i64, sid: &str, command: &str, reason: &str| {
        seq_row(
            ts,
            sid,
            RULE_READ_ONLY,
            Outcome::Approved,
            (command, reason),
            1,
        )
    };
    let unproven = "unproven: not read-only: touch";
    let warned = format!(
        "unproven: the rm circuit breaker (rm -rf x) \u{2014} {WARNED_UNDER_SAFE} ([harness] \
         rm_breaker_ask = true hands it to a person)"
    );
    let s1 = vec![
        press(t, "s-1", "touch a\nmore", unproven),
        press(t + 30_000, "s-1", "touch a\nother", unproven),
        press(t + 40_000, "s-1", "ls -la", unproven),
        press(t + 89_000, "s-1", "touch a\nmore", unproven),
        press(t + 150_000, "s-1", "touch a\nmore", unproven),
        press(t + 160_000, "s-1", "cargo test", ""),
        seq_row(
            t + 170_000,
            "s-1",
            RULE_ANSWER_RECOMMENDED,
            Outcome::Approved,
            ("Q? \u{2192} A", unproven),
            1,
        ),
        press(t + 180_000, "s-1", "rm -rf x", &warned),
    ];
    let s3 = vec![press(t + 155_000, "s-3", "touch a\nmore", unproven)];
    let vendor = "unproven: the box carries a vendor note: This command requires approval";
    let s4 = vec![
        press(t + 2_000, "s-4", "python3 - <<'PY' print(1) PY", vendor),
        press(t + 3_000, "s-4", "python3 - <<'PY' print(2) PY", vendor),
        press(t + 4_000, "s-4", "python3 - <<'PY'\nprint(4)\nPY", vendor),
        press(t + 5_000, "s-4", "cat <<<hi", vendor),
    ];
    let (_root, state, harness) =
        decided_tree(&[("s-1.jsonl", s1), ("s-3.jsonl", s3), ("s-4.jsonl", s4)]);
    let (today, _) = stats_windows(&state, &harness, t - 60_000, t - 86_400_000);
    let rows: Vec<(String, i64, String, u32, Option<String>)> = today
        .decided
        .expect("decisions")
        .rows
        .iter()
        .map(|d| {
            (
                d.sid.clone(),
                d.t_ms - t,
                d.asked.to_string(),
                d.times,
                d.why.as_deref().map(str::to_string),
            )
        })
        .collect();
    let all = Some("Approve permission boxes: All".to_string());
    assert_eq!(
        rows,
        [
            (
                "s-1".to_string(),
                0,
                "touch a \u{2026}".to_string(),
                3,
                all.clone()
            ),
            (
                "s-4".to_string(),
                2_000,
                "python3 - <<'PY' \u{2026}".to_string(),
                3,
                all.clone()
            ),
            (
                "s-4".to_string(),
                5_000,
                "cat <<<hi".to_string(),
                1,
                all.clone()
            ),
            (
                "s-1".to_string(),
                40_000,
                "ls -la".to_string(),
                1,
                all.clone()
            ),
            (
                "s-1".to_string(),
                150_000,
                "touch a \u{2026}".to_string(),
                1,
                all.clone()
            ),
            (
                "s-3".to_string(),
                155_000,
                "touch a \u{2026}".to_string(),
                1,
                all
            ),
            (
                "s-1".to_string(),
                180_000,
                "rm -rf x".to_string(),
                1,
                Some("Approve permission boxes: Safe, with the rm warning".to_string())
            ),
        ],
        "{rows:#?}"
    );
}

#[test]
fn the_cap_keeps_the_newest_decisions() {
    let t = now_ms() - 3_600_000;
    let decline = |k: i64, sid: &str| {
        seq_row(
            t + k * 1_000,
            sid,
            "decline@v1",
            Outcome::Declined,
            (&format!("buy {k}"), "-"),
            1,
        )
    };
    let a: Vec<String> = (0..4).chain(40..70).map(|k| decline(k, "s-a")).collect();
    let b: Vec<String> = (4..20).map(|k| decline(k, "s-b")).collect();
    let c: Vec<String> = (20..40).map(|k| decline(k, "s-c")).collect();
    let (_root, state, harness) =
        decided_tree(&[("s-a.jsonl", a), ("s-b.jsonl", b), ("s-c.jsonl", c)]);
    let (today, _) = stats_windows(&state, &harness, t - 60_000, t - 86_400_000);
    let decided = today.decided.expect("decisions");
    assert_eq!(decided.total, 70);
    assert_eq!(decided.rows.len(), DECIDED_KEEP);
    let kept: Vec<i64> = decided.rows.iter().map(|d| (d.t_ms - t) / 1_000).collect();
    assert_eq!(kept, (6..70).collect::<Vec<i64>>());
    assert!(
        decided.rows.iter().all(|d| d.why.is_none()),
        "a `-` reason is none"
    );
    assert!(!kept.iter().any(|k| *k < 6));
}

#[test]
fn the_agents_words_are_one_line_without_escapes_and_cut() {
    let raw = "\u{1b}[31mred\u{1b}[0m\nline\ttwo \u{202e}evil\u{1b}]0;title\u{7} end\u{7f}";
    assert_eq!(&*agent_words(raw), "red line two evil end");
    let long: String = "\u{e9}".repeat(200);
    let cut = agent_words(&long);
    assert_eq!(cut.chars().count(), DECIDED_CHARS + 1);
    assert!(cut.ends_with('\u{2026}'));
    assert!(cut.chars().take(DECIDED_CHARS).all(|c| c == '\u{e9}'));
    assert_eq!(&*agent_words("short"), "short");
    let alone = atpkg::progress::sanitize_for_tty(raw, DECIDED_CHARS);
    assert!(
        alone.contains("[31m") && alone.contains("linetwo"),
        "{alone}"
    );
}

#[test]
fn a_day_without_a_choice_has_no_decision() {
    let t = now_ms() - 3_600_000;
    let quiet = vec![
        seq_row(
            t,
            "s-1",
            RULE_CONTINUE,
            Outcome::Typed,
            ("keep going", "-"),
            1,
        ),
        seq_row(
            t + 1,
            "s-1",
            RULE_ALLOW_ONCE,
            Outcome::Approved,
            ("cargo test", ""),
            2,
        ),
    ];
    let (_root, state, harness) = decided_tree(&[("s-1.jsonl", quiet.clone())]);
    let (today, _) = stats_windows(&state, &harness, t - 60_000, t - 86_400_000);
    let decided = today.decided.expect("decisions");
    assert!(decided.rows.is_empty() && decided.total == 0);
    let mut busy = quiet;
    busy.push(seq_row(
        t + 2,
        "s-1",
        "decline@v1",
        Outcome::Declined,
        ("x", "no"),
        3,
    ));
    let (_root, state, harness) = decided_tree(&[("s-1.jsonl", busy)]);
    let (today, _) = stats_windows(&state, &harness, t - 60_000, t - 86_400_000);
    assert_eq!(today.decided.expect("decisions").total, 1, "CONTROL");
}

#[test]
fn a_prose_answer_joins_the_point_of_its_own_run() {
    let t = now_ms() - 3_600_000;
    let s = |k: i64| t + k * 1_000;
    let told = journal_lines(&format!(
        "{} s-1 EVENT question seq=5 \u{23fa} Which database should I use?
         {} s-1 EXIT stopped
         {} s-1 EVENT prompt seq=5 kind=other classify=- command=-
         {} s-1 EVENT idle seq=900 Old words.
         {} s-1 EVENT idle seq=7 \u{23fa} Shall I go on with the new run?
         {} s-1 APPROVED seq=9 Which colour? \u{2192} Blue
         {} s-1 DIALOG seq=8 rule=answer-recommended@v1 Which colour?
         {} s-1 EVENT question seq=10 Old question?
         {} s-1 EVENT prompt seq=4 kind=bash classify=- command=ls
         {} s-1 EVENT question seq=20 Before the exit?
         {} s-1 EXIT stopped
         {} s-1 EVENT idle seq=2 After everything.",
        s(0),
        s(50),
        s(60),
        s(100),
        s(200),
        s(210),
        s(211),
        s(400),
        s(500),
        s(600),
        s(650),
        s(800),
    ));
    let answer = |at: i64, seq: u64| {
        seq_row(
            s(at),
            "s-1",
            RULE_ANSWER,
            Outcome::Typed,
            ("Decide for yourself.", "the turn-end policy"),
            seq,
        )
    };
    let ledger = vec![
        answer(10, 6),
        answer(250, 7),
        answer(510, 12),
        answer(700, 25),
    ];
    let (_root, state, harness) =
        decided_tree(&[("s-1.journal.jsonl", told), ("s-1.jsonl", ledger)]);
    let (today, _) = stats_windows(&state, &harness, t - 60_000, t - 86_400_000);
    let rows: Vec<(i64, String, bool)> = today
        .decided
        .expect("decisions")
        .rows
        .iter()
        .map(|d| ((d.t_ms - t) / 1_000, d.asked.to_string(), d.said))
        .collect();
    assert_eq!(
        rows,
        [
            (10, "Which database should I use?".to_string(), false),
            (250, "Shall I go on with the new run?".to_string(), true),
            (510, String::new(), false),
            (700, String::new(), false),
        ]
    );
}

#[test]
fn an_unproven_press_names_the_option_it_chose() {
    use crate::supervise::policy::approval::{RULE_DECLINE, RULE_NO_SPEND, RULE_TRUST_ANY};
    let t = now_ms() - 3_600_000;
    let border = "\u{2594}".repeat(100);
    let long = "x".repeat(120);
    let press = |ts: i64, rule: &str, command: &str| {
        seq_row(
            ts,
            "s-1",
            rule,
            Outcome::Approved,
            (command, "unproven: the safe rules do not judge this box"),
            1,
        )
    };
    let spend = format!("{border} => Stop and wait for limit to reset");
    let not_now = format!("{long} => Not now");
    let ledger = vec![
        press(t, RULE_DECLINE, "Enable the new model? => No, keep default"),
        press(t + 1_000, RULE_NO_SPEND, &spend),
        press(t + 2_000, RULE_NO_SPEND, &spend),
        press(t + 3_000, RULE_ALLOW_ONCE, "node -e 'f(a => b)'"),
        press(t + 4_000, RULE_TRUST_ANY, "/Users//x/proj => y"),
        press(t + 5_000, RULE_DECLINE, &not_now),
    ];
    let (_root, state, harness) = decided_tree(&[("s-1.jsonl", ledger)]);
    let (today, _) = stats_windows(&state, &harness, t - 60_000, t - 86_400_000);
    let rows: Vec<(String, String, u32)> = today
        .decided
        .expect("decisions")
        .rows
        .iter()
        .map(|d| (d.asked.to_string(), d.did.to_string(), d.times))
        .collect();
    let cut = agent_words(&long).to_string();
    assert_eq!(
        rows,
        [
            (
                "Enable the new model?".to_string(),
                "No, keep default".to_string(),
                1
            ),
            (
                String::new(),
                "Stop and wait for limit to reset".to_string(),
                2
            ),
            ("node -e 'f(a => b)'".to_string(), String::new(), 1),
            ("/Users//x/proj => y".to_string(), String::new(), 1),
            (cut.clone(), "Not now".to_string(), 1),
        ]
    );
    assert!(cut.ends_with('\u{2026}'));
    for command in [&spend, &not_now] {
        assert!(!agent_words(command).contains("=>"), "{command}");
    }
}

#[test]
fn a_run_is_presses_of_one_command_under_one_switch() {
    let t = now_ms() - 3_600_000;
    let prefix = format!("S=/private/tmp/{}/scratchpad/probe;", "p".repeat(90));
    let first = format!("{prefix} L=1 run");
    let second = format!("{prefix} mkdir -p $S");
    let all = "unproven: not read-only: run";
    let warned =
        format!("unproven: the rm circuit breaker (rm -rf x) \u{2014} {WARNED_UNDER_SAFE}");
    let press = |ts: i64, command: &str, reason: &str| {
        seq_row(
            ts,
            "s-1",
            RULE_READ_ONLY,
            Outcome::Approved,
            (command, reason),
            1,
        )
    };
    let ledger = vec![
        press(t, &first, all),
        press(t + 1_000, &second, all),
        press(t + 2_000, &first, all),
        press(t + 3_000, "rm -rf x", all),
        press(t + 4_000, "rm -rf x", &warned),
    ];
    let (_root, state, harness) = decided_tree(&[("s-1.jsonl", ledger)]);
    let (today, _) = stats_windows(&state, &harness, t - 60_000, t - 86_400_000);
    let rows: Vec<(i64, u32, Option<String>)> = today
        .decided
        .expect("decisions")
        .rows
        .iter()
        .map(|d| (d.t_ms - t, d.times, d.why.as_deref().map(str::to_string)))
        .collect();
    let all_words = Some("Approve permission boxes: All".to_string());
    assert_eq!(
        rows,
        [
            (0, 2, all_words.clone()),
            (1_000, 1, all_words.clone()),
            (3_000, 1, all_words),
            (
                4_000,
                1,
                Some("Approve permission boxes: Safe, with the rm warning".to_string())
            ),
        ]
    );
    assert_eq!(command_head(&first).1, command_head(&second).1);
}

#[test]
fn a_decisions_id_stays_while_newer_rows_arrive() {
    let t = now_ms() - 3_600_000;
    let decline =
        |ts: i64, what: &str| seq_row(ts, "s-1", "decline@v1", Outcome::Declined, (what, "no"), 1);
    let early = vec![decline(t, "a"), decline(t + 20_000, "b")];
    let (_root, state, harness) = decided_tree(&[("s-1.jsonl", early.clone())]);
    let (first, _) = stats_windows(&state, &harness, t - 60_000, t - 86_400_000);
    let first = first.decided.expect("decisions").rows;
    let mut later = early;
    later.insert(
        1,
        seq_row(
            t + 10_000,
            "s-1",
            RULE_ANSWER_RECOMMENDED,
            Outcome::Approved,
            (
                "Late? \u{2192} Yes",
                "unproven: answered with its recommended option",
            ),
            1,
        ),
    );
    later.push(decline(t + 30_000, "c"));
    let told = journal_lines(&format!(
        "{} s-1 CHOSE seq=9 rule=answer-recommended@v1 policy=recommended Late? \u{2192} Yes",
        t + 40_000
    ));
    let (_root, state, harness) =
        decided_tree(&[("s-1.journal.jsonl", told), ("s-1.jsonl", later)]);
    let (second, _) = stats_windows(&state, &harness, t - 60_000, t - 86_400_000);
    let second = second.decided.expect("decisions").rows;
    assert_eq!(second.len(), 4);
    let place = |rows: &[Decision], id: &str| rows.iter().position(|d| d.id() == id);
    for decision in &first {
        assert!(
            place(&second, &decision.id()).is_some(),
            "{decision:?} keeps its id"
        );
    }
    let ids: BTreeSet<String> = second.iter().map(Decision::id).collect();
    assert_eq!(ids.len(), 4, "every id its own");
    let one = &first[0];
    for other in [
        Decision {
            t_ms: one.t_ms + 1,
            ..one.clone()
        },
        Decision {
            sid: "s-2".to_string(),
            ..one.clone()
        },
        Decision {
            kind: DecisionKind::KeptReset,
            ..one.clone()
        },
    ] {
        assert_ne!(other.id(), one.id(), "{other:?}");
    }
    assert_eq!(one.id().len(), 16);
    let b = first[1].id();
    assert_eq!((place(&first, &b), place(&second, &b)), (Some(1), Some(2)));
}

#[test]
fn a_kept_reset_says_why_in_words_and_a_cut_answer_closes_its_quote() {
    let t = now_ms() - 3_600_000;
    let kept = |ts: i64, why: &str| {
        seq_row(
            ts,
            "s-1",
            RULE_LIMIT_RESET,
            Outcome::Declined,
            ("Checking for a reset", why),
            1,
        )
    };
    let text = format!(
        "Nobody is here to answer. {}",
        "Decide for yourself. ".repeat(8)
    );
    let ledger = vec![
        kept(t, "check-timeout"),
        kept(t + 1_000, "inexact-title"),
        kept(t + 2_000, "frobbed"),
        seq_row(
            t + 3_000,
            "s-1",
            RULE_ANSWER,
            Outcome::Typed,
            (&text, "the turn-end policy"),
            1,
        ),
    ];
    let (_root, state, harness) = decided_tree(&[("s-1.jsonl", ledger)]);
    let (today, _) = stats_windows(&state, &harness, t - 60_000, t - 86_400_000);
    let rows = today.decided.expect("decisions").rows;
    let kept: Vec<(DecisionKind, &str, Option<&str>)> = rows[..3]
        .iter()
        .map(|d| (d.kind, &*d.asked, d.why.as_deref()))
        .collect();
    assert_eq!(
        kept,
        [
            (
                DecisionKind::KeptReset,
                "",
                Some("the check for a reset took longer than it may")
            ),
            (
                DecisionKind::KeptReset,
                "",
                Some("its confirmation did not read exactly as expected (its title)")
            ),
            (DecisionKind::KeptReset, "", None),
        ]
    );
    let answer = &rows[3].did;
    assert!(
        answer.starts_with("the answer text \u{201c}Nobody is here to answer.")
            && answer.ends_with("\u{2026}\u{201d}"),
        "{answer}"
    );
    assert!(answer.chars().count() <= DECIDED_CHARS + 1, "{answer}");
    assert!(text.chars().count() > DECIDED_CHARS);
}

#[test]
fn a_burst_stays_separate_across_the_whole_timestamp_domain() {
    let mut acc = HarnessStats::per_session(i64::MIN);
    for ts in [i64::MIN, i64::MIN + 1, i64::MAX] {
        fold_ledger_line(
            &mut acc,
            &seq_row(
                ts,
                "s-1",
                RULE_READ_ONLY,
                Outcome::Approved,
                ("touch a", "unproven: not read-only: touch"),
                1,
            ),
        );
    }
    acc.end_reading();
    let decided = acc.decided.expect("decisions");
    let rows: Vec<(i64, u32)> = decided.rows.iter().map(|d| (d.t_ms, d.times)).collect();
    assert_eq!(decided.total, 2);
    assert_eq!(rows, [(i64::MIN, 2), (i64::MAX, 1)]);
}

#[test]
fn a_need_told_is_counted_apart_from_escalations_and_waits() {
    let recs = journal(
        "
        1000 s-1 ESCALATED seq=10 attention=OK mail=skipped: no fabric
        2000 s-1 NEEDS seq=11 count=1 attention=OK the App Store signing credential
        3000 s-1 CLEARED seq=12 needs attention=OK none stands now
        3500 s-1 NEEDS seq=13 count=2 attention=ERR attention too long (max 10 bytes) x
        4000 s-1 CLEARED seq=14 box attention=OK
        5000 s-2 NEEDS seq=3 count=1 attention=OK a decision on the launch date
    ",
    );
    let mut acc = HarnessStats::new(0);
    fold_journal(&mut acc, &recs);
    assert_eq!(acc.needs_told, 2, "{acc:#?}");
    assert_eq!((acc.escalated, acc.human_wait_ms), (1, 3_000), "{acc:#?}");
    let mut acc = HarnessStats::new(2_500);
    fold_journal(&mut acc, &recs);
    assert_eq!(
        acc.needs_told, 1,
        "only the one inside the window: {acc:#?}"
    );
}

fn land_note(t: i64, sid: &str, why: &str, ended: Option<i64>) -> JournalRecord {
    let mut r = JournalRecord::of_line(
        t,
        Some(sid),
        &format!(
            "DONE seq=104 rule={RULE_LAND} {why} (autopilot land): nothing more is typed until \
             a person or an orchestrator types"
        ),
        None,
    );
    r.ended = ended;
    r
}

const DEFENDED: &str = "the worker defended that everything is landed on the remote main branch";

#[test]
fn a_landing_counts_once_through_a_restatement_and_a_wake() {
    let mut recs = journal(&format!(
        "1000 s-1 CONTINUED seq=100 rule={RULE_LAND} [aterm harness] Autopilot is Land: land ALL of this work
         1500 s-1 EVENT idle seq=104 - Nothing left: all of it is merged into the remote main branch."
    ));
    recs.push(land_note(2_000, "s-1", DEFENDED, None));
    recs.push(land_note(3_000, "s-1", WAKE_KEPT, Some(2_000)));
    recs.extend(journal("4000 s-1 EXIT stopped"));
    recs.push(land_note(5_000, "s-1", DEFENDED, Some(2_000)));
    recs.extend(journal(
        "6000 s-2 DONE seq=110 rule=continue-done-check@v1 the worker said it is done after the done check
         6500 s-2 DONE seq=111 rule=finished@v1 the worker reported ATERM-TASK-DONE: its task is finished",
    ));
    let notes = recs
        .iter()
        .filter(|r| r.line.starts_with("DONE ") && r.line.contains(&format!("rule={RULE_LAND} ")))
        .count();
    assert_eq!(notes, 3, "CONTROL: three notes of the one landing");
    let mut acc = HarnessStats::per_session(0);
    fold_journal(&mut acc, &recs);
    for line in [
        row(
            6_100,
            "s-2",
            RULE_DONE_CHECK,
            Outcome::Typed,
            "Before you stop: …",
            "-",
        ),
        row(
            6_200,
            "s-2",
            approvals::RULE_TASK_DONE,
            Outcome::Waiting,
            "",
            approvals::TASK_DONE,
        ),
        row(900, "s-1", RULE_LAND, Outcome::Typed, "land it", "-"),
    ] {
        fold_ledger_line(&mut acc, &line);
    }
    acc.end_reading();
    assert_eq!(acc.landed, 1, "{acc:#?}");
    let shares = acc.by_session.as_ref().expect("shares");
    assert_eq!(shares["s-1"].landed, 1);
    assert_eq!(shares["s-2"].landed, 0, "a Cruise done claim is no landing");
    assert_eq!(
        (acc.land_prompts, acc.continued),
        (1, 1),
        "the land prompt and the done check, each its own count"
    );
    let mut later = HarnessStats::new(2_001);
    fold_journal(&mut later, &recs);
    assert_eq!(later.landed, 0, "{later:#?}");
    let mut at = HarnessStats::new(2_000);
    fold_journal(&mut at, &recs);
    assert_eq!(at.landed, 1, "a note at the window's start is inside it");
}

#[test]
fn each_landing_counts_and_an_older_builds_notes_read_as_the_landings_they_were() {
    let older = "the worker said it is done after the land instruction";
    let act = |t: i64, sid: &str, rule: &str| {
        JournalRecord::of_line(
            t,
            Some(sid),
            &format!("CONTINUED seq=7 rule={rule} [aterm harness] Autopilot is set to Land"),
            None,
        )
    };
    let exit = |t: i64, sid: &str| JournalRecord::of_line(t, Some(sid), "EXIT stopped", None);
    let recs = vec![
        act(1_000, "s-a", RULE_LAND),
        land_note(2_000, "s-a", older, None),
        exit(2_100, "s-a"),
        land_note(3_000, "s-a", older, None),
        exit(3_100, "s-a"),
        land_note(4_000, "s-a", older, None),
        act(5_000, "s-b", RULE_LAND_ANSWER),
        act(5_100, "s-b", RULE_LAND),
        land_note(6_000, "s-b", older, None),
        act(7_000, "s-b", RULE_LAND),
        land_note(8_000, "s-b", older, None),
        act(9_000, "s-b", RULE_LAND_NUDGE),
        land_note(10_000, "s-b", older, None),
        land_note(11_000, "s-c", DEFENDED, None),
        land_note(12_000, "s-d", DEFENDED, Some(11_500)),
        act(13_000, "s-e", RULE_LAND),
        act(14_000, "s-e", RULE_LAND_NUDGE),
        act(15_000, "s-f", RULE_LAND),
        land_note(16_000, "s-f", older, None),
        JournalRecord::of_line(
            16_500,
            Some("s-f"),
            &format!("TYPED seq=8 rule={RULE_USAGE_PROBE} /usage"),
            None,
        ),
        JournalRecord::of_line(
            16_600,
            Some("s-f"),
            &format!(
                "CONTINUED seq=9 rule={RULE_CONTINUE} {}",
                clipped(CONTINUE_TEXT)
            ),
            None,
        ),
        exit(16_700, "s-f"),
        land_note(17_000, "s-f", older, None),
        land_note(18_000, "s-g", DEFENDED, Some(11_500)),
        exit(18_100, "s-g"),
        land_note(19_000, "s-g", older, None),
    ];
    let a_notes: Vec<&JournalRecord> = recs
        .iter()
        .filter(|r| r.sid.as_deref() == Some("s-a") && r.line.starts_with("DONE "))
        .collect();
    assert_eq!(a_notes.len(), 3);
    assert!(
        a_notes
            .iter()
            .all(|r| r.ended.is_none() && !r.line.contains(WAKE_KEPT)),
        "CONTROL: the older build's notes look like landings of their own"
    );
    let mut acc = HarnessStats::per_session(0);
    fold_journal(&mut acc, &recs);
    acc.end_reading();
    let shares = acc.by_session.as_ref().expect("shares");
    let landed = |sid: &str| shares.get(sid).map_or(0, |t| t.landed);
    assert_eq!(
        [
            landed("s-a"),
            landed("s-b"),
            landed("s-c"),
            landed("s-d"),
            landed("s-e"),
            landed("s-f"),
            landed("s-g")
        ],
        [1, 3, 1, 0, 0, 1, 0],
        "{shares:#?}"
    );
    assert_eq!(acc.landed, 6);
}

fn clipped(text: &str) -> String {
    text.chars().take(LINE_CHARS).collect()
}

#[test]
fn a_walls_continuation_in_lands_words_is_the_land_act_a_landing_comes_at() {
    let rec = |t: i64, sid: &str, line: String| JournalRecord::of_line(t, Some(sid), &line, None);
    let mut recs = Vec::new();
    let mut sids = Vec::new();
    for (i, rule) in [RULE_LIMIT_RESUME, RULE_CONSENT, RULE_COMPACT]
        .into_iter()
        .enumerate()
    {
        for (j, words) in [LAND, LAND_NUDGE].into_iter().enumerate() {
            let sid = format!("s-{i}{j}");
            let t = i64::try_from(10 * (2 * i + j)).expect("small") * 1_000;
            recs.extend([
                rec(
                    t + 1_000,
                    &sid,
                    format!("CONTINUED seq=100 rule={RULE_LAND} {}", clipped(LAND)),
                ),
                land_note(t + 2_000, &sid, DEFENDED, None),
                rec(
                    t + 3_000,
                    &sid,
                    "EVENT turn seq=150 Added the flag.".to_string(),
                ),
                rec(
                    t + 4_000,
                    &sid,
                    format!("CONTINUED seq=200 rule={rule} {}", clipped(words)),
                ),
                land_note(t + 5_000, &sid, DEFENDED, None),
            ]);
            sids.push(sid);
        }
    }
    for (sid, words) in [
        ("s-other", CONTINUE_TEXT),
        ("s-retired", LAND_NUDGE_COMMITTED_BEFORE),
    ] {
        recs.extend([
            rec(
                70_000,
                sid,
                format!("CONTINUED seq=100 rule={RULE_LAND} {}", clipped(LAND)),
            ),
            land_note(71_000, sid, DEFENDED, None),
            rec(
                72_000,
                sid,
                format!(
                    "CONTINUED seq=200 rule={RULE_LIMIT_RESUME} {}",
                    clipped(words)
                ),
            ),
            land_note(73_000, sid, DEFENDED, None),
        ]);
    }
    let live: Vec<JournalRecord> =
        include_str!("../supervise/fixtures/muse-1.4.11-journal-land-at-wall-resumes.jsonl")
            .lines()
            .map(|l| JournalRecord::from_json(l).expect("a journal record"))
            .collect();
    let resumes = live
        .iter()
        .filter(|r| r.kind == "continued" && r.phase == RULE_LIMIT_RESUME)
        .count();
    let notes = live
        .iter()
        .filter(|r| r.line.starts_with("DONE seq=") && r.ended.is_none())
        .count();
    assert_eq!(
        (resumes, notes),
        (2, 3),
        "CONTROL: the run's acts and notes"
    );
    let mut acc = HarnessStats::per_session(0);
    fold_journal(&mut acc, &recs);
    acc.end_reading();
    let shares = acc.by_session.as_ref().expect("shares");
    let landed = |sid: &str| shares.get(sid).map_or(0, |t| t.landed);
    let mut live_acc = HarnessStats::per_session(0);
    fold_journal(&mut live_acc, &live);
    let got = (
        sids.iter().map(|sid| landed(sid)).collect::<Vec<_>>(),
        landed("s-other"),
        landed("s-retired"),
        acc.landed,
        live_acc.landed,
    );
    assert_eq!(got, (vec![2; 6], 1, 1, 2 * 6 + 2, 3), "{shares:#?}");
}

#[test]
fn a_fact_line_typed_counts_as_no_act_and_ends_no_wait() {
    let fact_rule = crate::supervise::facts::FactKind::Hold.rule();
    let wall = row(
        1_000,
        "s-1",
        RULE_WALL_WAIT,
        Outcome::Waiting,
        "",
        "(wall: …)",
    );
    let fact = typed(2_000, fact_rule, "the line");
    let resume = typed(5_000, RULE_LIMIT_RESUME, "go on");
    let lines = [wall, fact, resume];
    let acc = fold_ledger(&lines, 0);
    assert_eq!(
        (by(&acc, fact_rule), acc.limit_wait_ms, acc.rows),
        (0, 4_000, 3),
        "the fact row is a row, and no act"
    );
    let counted = approvals::with_facts_counted(|| fold_ledger(&lines, 0));
    assert_eq!(
        (by(&counted, fact_rule), counted.limit_wait_ms),
        (1, 1_000),
        "read as an act, it counts and ends the wait"
    );
}

#[test]
fn the_fact_lines_count_by_kind() {
    use crate::supervise::occupancy::door::INTENT_RULE;
    use crate::supervise::occupancy::rules::{HELD_RULE, WOULD_RULE};
    let hold = crate::supervise::facts::FactKind::Hold.rule();
    let lines = [
        said(
            500,
            WOULD_RULE,
            Outcome::Waiting,
            "rule=wait-nudge@v1 episode=00000000000000aa held_ms=1800000",
        ),
        said(
            1_000,
            WOULD_RULE,
            Outcome::Waiting,
            "rule=wait-nudge@v1 episode=00000000000000ab held_ms=1800000",
        ),
        said(
            2_000,
            WOULD_RULE,
            Outcome::Waiting,
            "rule=hold-room@v1 episode=00000000000000ac held_ms=7200000",
        ),
        said(
            2_500,
            WOULD_RULE,
            Outcome::Waiting,
            "rule=wake@v9 episode=00000000000000ad held_ms=1",
        ),
        said(
            3_000,
            INTENT_RULE,
            Outcome::Waiting,
            "rule=hold-room@v1 episode=00000000000000ae held_ms=7200000",
        ),
        typed(3_100, hold, "the line"),
        said(
            3_200,
            hold,
            Outcome::Skipped,
            "rule=hold-room@v1 episode=00000000000000af held_ms=7200000 why=unfenced",
        ),
        said(3_300, hold, Outcome::Waiting, "hold=open held_ms=600000"),
        said(
            3_400,
            HELD_RULE,
            Outcome::Waiting,
            "rule=hold-room@v1 episode=00000000000000b0 held_ms=7200000 ordinal=0 guard=person",
        ),
    ];
    let acc = fold_ledger(&lines, 1_000);
    let one = |n: u64| std::collections::BTreeMap::from([("hold".to_string(), n)]);
    assert_eq!(acc.facts_would, one(2), "{acc:#?}");
    assert_eq!(acc.facts_told, one(1), "{acc:#?}");
    assert_eq!(acc.by_rule.get(hold), None, "still no act");
    assert_eq!(acc.by_rule.get(HELD_RULE), None, "a held row is no act");
}

/// The task door's rows (`door=task`) are no line on the Harness page: under `observe` a managed
/// worker's would-be note, and under `prompt` its intent and outcome, count toward neither
/// `facts_would` nor `facts_told`. CONTROL: the same would-be row with no task door's mark counts
/// as a line that would have been typed.
#[test]
fn the_task_doors_rows_count_no_line() {
    use crate::supervise::occupancy::door::{INTENT_RULE, TASK_DOOR};
    use crate::supervise::occupancy::rules::WOULD_RULE;
    let hold = crate::supervise::facts::FactKind::Hold.rule();
    let words = "rule=wait-nudge@v1 episode=00000000000000aa held_ms=1800000";
    let would = |reason: &str| said(500, WOULD_RULE, Outcome::Waiting, reason);
    let noted = [
        would(&format!("{words} {TASK_DOOR}")),
        said(
            600,
            INTENT_RULE,
            Outcome::Waiting,
            &format!("{words} {TASK_DOOR}"),
        ),
        said(
            700,
            hold,
            Outcome::Waiting,
            &format!("{words} {TASK_DOOR} noted"),
        ),
        said(
            800,
            hold,
            Outcome::Skipped,
            &format!("{words} {TASK_DOOR} why=no-task"),
        ),
    ];
    let acc = fold_ledger(&noted, 0);
    assert_eq!(
        acc.facts_would,
        std::collections::BTreeMap::new(),
        "{acc:#?}"
    );
    assert_eq!(
        acc.facts_told,
        std::collections::BTreeMap::new(),
        "{acc:#?}"
    );
    let acc = fold_ledger(&[would(words)], 0);
    assert_eq!(
        acc.facts_would,
        std::collections::BTreeMap::from([("hold".to_string(), 1)]),
        "CONTROL: a would-be line's row counts"
    );
}

/// A go-ahead's decision row names the owner's standing go-ahead and its limits, never the typed
/// words, which quote the agent's own park.
#[test]
fn a_go_ahead_decision_says_the_owners_standing_go_ahead() {
    let typed = "[aterm harness] Proceed now with your own work: build, test, commit…";
    assert_eq!(&*prose_answer_words(RULE_GO_AHEAD, typed), GO_AHEAD_DID);
    assert_eq!(
        crate::harness::acts::self_claim_act(RULE_GO_AHEAD).map(|(word, _)| word),
        Some("answer")
    );
}
