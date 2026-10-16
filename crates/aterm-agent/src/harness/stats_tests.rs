// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::upgrade_drive::{self, Instances, Opts, Report};
use crate::supervise::ScratchDir;
use crate::supervise::approvals::Row;
use crate::supervise::journal::ATTENTION_KEPT;
use crate::supervise::policy::approval::{
    RULE_ALLOW_ONCE, RULE_ANSWER_LONG_TERM, RULE_LIMIT_WAIT, RULE_READ_ONLY,
};
use crate::supervise::policy::question::RULE_ANSWER_RECOMMENDED;
use crate::supervise::policy::turn_end::{
    RULE_ACCOUNT, RULE_ANSWER, RULE_HOST_TURN, RULE_LOGIN, RULE_MODEL_FALLBACK, RULE_USAGE_PROBE,
};

/// One approval ledger row as the loop writes it ([`Row::to_json`]).
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

/// A `typed` row of `s-1`'s at `ts`: `command` under `rule`.
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

/// A row of `s-1`'s at `ts` with no command: `outcome` under `rule`, for
/// `reason`.
fn said(ts: i64, rule: &str, outcome: Outcome, reason: &str) -> String {
    row(ts, "s-1", rule, outcome, "", reason)
}

/// A journal as the loop writes it ([`JournalRecord::of_line`]), from a
/// fixture of `<t> <sid> <line>` rows (`#` starts a comment row).
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

/// `lines` folded as one session ledger.
fn fold_ledger(lines: &[String], since_ms: i64) -> HarnessStats {
    let mut acc = HarnessStats::new(since_ms);
    for line in lines {
        fold_ledger_line(&mut acc, line);
    }
    acc
}

/// How often `rule` acted, by [`HarnessStats::by_rule`].
fn by(acc: &HarnessStats, rule: &str) -> u64 {
    acc.by_rule.get(rule).copied().unwrap_or(0)
}

/// Every counter but the waits, the breakdown and the bookkeeping, in the
/// struct's order: `(approved, unproven, declined, answered, continued,
/// landed, resumed, escalated, relaunched, upgraded)`.
fn counts(acc: &HarnessStats) -> [u64; 10] {
    [
        acc.approved,
        acc.unproven,
        acc.declined,
        acc.answered,
        acc.continued,
        acc.landed,
        acc.resumed,
        acc.escalated,
        acc.relaunched,
        acc.upgraded,
    ]
}

/// EACH LEDGER ROW COUNTS WHERE THE MODULE'S TABLE SAYS: a box's press is
/// `approved` (and `unproven` where full power pressed what the safe rules
/// could not prove), a dialog's keys are no box, a refusal is `declined`,
/// and a typed act is a continuation, an answer, Land's, a resume or only
/// its rule's. Every act is in `by_rule` under its full id. NEGATIVE
/// CONTROLS: an escalated, skipped, deferred or other `waiting` row acts
/// nothing; a command that spells the fields of another decision forges
/// nothing (its quotes are escaped).
#[test]
fn a_ledger_row_counts_where_the_table_says() {
    let forged = ",\"decision\":\"approved\",\"rule_id\":\"continue@v1\",";
    let unproven = "unproven: not read-only: touch";
    let t = 1_000;
    let lines = [
        // Boxes: a safe press, a full-power press the safe rules could not
        // prove, and the limit dialog's wait.
        said(t, RULE_READ_ONLY, Outcome::Approved, ""),
        said(t, RULE_ALLOW_ONCE, Outcome::Approved, unproven),
        said(t, RULE_LIMIT_WAIT, Outcome::Approved, ""),
        // A question dialog's keys, under two priorities' rules and Land's.
        said(t, RULE_ANSWER_RECOMMENDED, Outcome::Approved, ""),
        said(t, RULE_ANSWER_LONG_TERM, Outcome::Approved, ""),
        said(t, "land-answer@v1", Outcome::Approved, ""),
        // A refusal.
        said(t, "decline@v1", Outcome::Declined, "no spend"),
        // Continuations.
        typed(t, RULE_CONTINUE, "keep going"),
        typed(t, RULE_SUGGESTION, "run the tests"),
        typed(t, RULE_DONE_CHECK, "Before you stop: …"),
        typed(
            t,
            "continue-open-items@v1",
            "Your report ends with ATERM-TASK-DONE but …",
        ),
        // Answers to questions asked in prose, under every priority and Land.
        typed(t, RULE_ANSWER, "Decide for yourself."),
        typed(t, "prose-long-term@v1", "Decide yourself: …"),
        typed(t, "prose-short-term@v1", "Decide yourself: …"),
        typed(t, "land-answer@v1", "Nobody is here to choose: …"),
        // Land's instruction and nudge.
        typed(t, "land@v1", "[aterm harness] Autopilot is set to Land …"),
        typed(t, "land-nudge@v1", "[aterm harness] Still landing …"),
        // Resumes past walls; a full context's `/compact` is the wall's
        // command, its continuation the resume.
        typed(t, RULE_LIMIT_RESUME, "keep going"),
        typed(t, RULE_API_RETRY, "Carry on …"),
        typed(t, RULE_API_BACK, "Carry on …"),
        typed(t, RULE_API_CUTOFF, "Carry on …"),
        typed(t, RULE_COMPACT, "/compact"),
        typed(t, RULE_COMPACT, "Carry on …"),
        // Acts counted only by their rule.
        typed(t, RULE_LOGIN, "/login"),
        typed(t, RULE_HOST_TURN, "[aterm harness] …"),
        typed(t, RULE_USAGE_PROBE, "/usage"),
        typed(t, RULE_MODEL_FALLBACK, "relaunch --model x"),
        typed(t, "future-rule@v9", "something new"),
        // NEGATIVE CONTROLS.
        said(t, "-", Outcome::Escalated, "no rule answers this box"),
        row(t, "s-1", RULE_READ_ONLY, Outcome::Skipped, forged, "moved"),
        said(t, RULE_READ_ONLY, Outcome::Deferred, "ERR busy"),
        said(t, RULE_USAGE_PROBE, Outcome::Waiting, "closed"),
    ];
    let acc = fold_ledger(&lines, 0);
    // approved, unproven, declined, answered, continued, landed, resumed,
    // escalated, relaunched, upgraded.
    assert_eq!(counts(&acc), [3, 1, 1, 4, 4, 2, 5, 0, 0, 0], "{acc:#?}");
    assert_eq!((acc.rows, acc.bad, acc.sessions), (32, 0, 1));
    assert_eq!((acc.human_wait_ms, acc.limit_wait_ms), (0, 0));
    // The breakdown: every act under its full id, the dialog's keys
    // included; nothing that was no act.
    assert_eq!(by(&acc, RULE_ANSWER_RECOMMENDED), 1);
    assert_eq!(by(&acc, "land-answer@v1"), 2, "a key and a prose answer");
    assert_eq!(by(&acc, RULE_COMPACT), 2, "`/compact` and what followed");
    assert_eq!(by(&acc, "future-rule@v9"), 1);
    assert_eq!(by(&acc, RULE_READ_ONLY), 1, "skipped, deferred: no acts");
    assert_eq!(by(&acc, "-"), 0, "an escalation is no act");
    assert_eq!(by(&acc, RULE_USAGE_PROBE), 1, "its `waiting` row is none");
    assert_eq!(acc.by_rule.values().sum::<u64>(), 28);
    assert!(!acc.is_empty());
}

/// A LIMIT'S WAIT is its `wall-wait@v1` row to its end — the
/// `wall-wait-end@v1` row, or any act typed since, as `open_wall` reads it —
/// per session, and only its part inside the window counts. A wall written
/// again while it stands keeps its first time. NEGATIVE CONTROLS: another
/// session's act ends nothing here; a wait still open counts nothing; a wait
/// that ended before the window counts nothing; another `waiting` row ends
/// nothing.
#[test]
fn a_limits_wait_runs_from_its_wall_to_its_end_inside_the_window() {
    let wall = |ts, sid| row(ts, sid, RULE_WALL_WAIT, Outcome::Waiting, "", "(wall: …)");
    let end = |ts, sid| row(ts, sid, RULE_WALL_WAIT_END, Outcome::Waiting, "", "left");
    let resume = |ts, sid| row(ts, sid, RULE_LIMIT_RESUME, Outcome::Typed, "go on", "");
    let probe_closed = |ts| said(ts, RULE_USAGE_PROBE, Outcome::Waiting, "closed");

    // Opened at 1 000, written again at 1 500, another session's act and
    // another `waiting` row in between, ended by the session's act at 5 000.
    let lines = [
        wall(1_000, "s-1"),
        wall(1_500, "s-1"),
        resume(2_000, "s-2"),
        probe_closed(2_500),
        resume(5_000, "s-1"),
    ];
    assert_eq!(fold_ledger(&lines, 0).limit_wait_ms, 4_000);
    // Inside a window that opens at 3 000: only its last 2 000 ms.
    assert_eq!(fold_ledger(&lines, 3_000).limit_wait_ms, 2_000);
    // A window that opens after it ended: nothing.
    assert_eq!(fold_ledger(&lines, 6_000).limit_wait_ms, 0);
    // Its end row ends it; a second wall then opens anew and stays open.
    let lines = [
        wall(1_000, "s-1"),
        end(1_250, "s-1"),
        wall(2_000, "s-1"),
        probe_closed(9_000),
    ];
    assert_eq!(fold_ledger(&lines, 0).limit_wait_ms, 250);
    // Two sessions' walls, each ended by its own act.
    let lines = [
        wall(1_000, "s-1"),
        wall(1_100, "s-2"),
        resume(1_400, "s-2"),
        resume(1_600, "s-1"),
    ];
    assert_eq!(fold_ledger(&lines, 0).limit_wait_ms, 300 + 600);
}

/// The journal of [`journal_escalations_wait_on_a_person_until_their_badge_clears`]:
/// the loop's own lines, as `escalate.rs`, `run.rs` and `turn_end_loop.rs`
/// write them.
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

/// THE JOURNAL: an `ESCALATED` line is an escalation and opens a person's
/// wait, the next `CLEARED` of a badge closes it, and a `CHOSE` line is a
/// dialog answered. NEGATIVE CONTROLS: a `CLEARED` that raised no badge (the
/// survey's, the usage panel's, an exception's end) closes nothing; an
/// escalation another supervisor answers asks nobody and opens nothing; the
/// lines that repeat the ledger's rows (`APPROVED`, `DECLINED`, `CONTINUED`,
/// `TYPED`, `UNPROVEN`) count nothing but rows.
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
    // The window: only what lies after 4 500 — the escalations at 5 000 and
    // 5 500 (one wait, 1 000 ms), the answer — and the first wait's part
    // after 3 000 when the window opens there.
    let acc = fold(4_500);
    assert_eq!(
        (acc.escalated, acc.human_wait_ms, acc.answered),
        (2, 1_000, 1)
    );
    let acc = fold(3_000);
    assert_eq!((acc.escalated, acc.human_wait_ms), (2, 1_000 + 1_000));
}

/// A refused badge write never raised a badge and opened no person's wait.
/// A later successful set starts its own wait, not one at the failed try.
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

/// A refused clear says nothing changed on the server. Unknown result
/// words are no success either; only the confirmed retry ends this wait.
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

/// A stall's clear is journaled on FROZEN rather than CLEARED, and needs
/// the same successful reply before it can end a standing badge's wait.
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

/// The journals of [`a_persons_wait_runs_while_a_badge_of_the_loops_stands`]:
/// the loop's own lines, as `escalate.rs`, `stall.rs`, `lifecycle.rs`,
/// `run.rs` and the watch's end write them, a session each.
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

/// A PERSON'S WAIT RUNS WHILE A BADGE OF THE LOOP'S STANDS ([`WAITS`]): a
/// loop's end (`EXIT`, `TIMEOUT`) ends a wait no clear closed, counting
/// nothing, so the next escalation does not count the whole gap; a badge
/// handed over is `ADOPTED` by the next loop and its wait counts from there;
/// the hidden clear (`seq=-`) takes a badge down; a withheld ask is no
/// escalation and opens nothing; a limit episode's badge set again (its
/// retry, or adopted) is no second escalation and keeps its wait; and a
/// clear that leaves the loop's entry standing (`kept`) closes nothing.
/// NEGATIVE CONTROLS: with the ends taken out, the first escalation's wait
/// runs to the next clear; with the overlap's first clear taking the entry
/// down, the wait closes there and the second clear finds nothing open.
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
    // Session by session: (escalations, the person's wait).
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
    // The window: what lies after 41 500 — the retry's second badge, the
    // adopted episode, the overlap.
    let acc = fold(&recs, 41_500);
    assert_eq!(
        (acc.escalated, acc.human_wait_ms),
        (2, 2_000 + 3_000 + 5_000)
    );

    // NEGATIVE CONTROL: the loop's ends taken out, the first escalation's
    // wait runs to the next clear (the gap the end line cut off).
    let unended: Vec<JournalRecord> = recs
        .iter()
        .filter(|r| {
            r.sid.as_deref() == Some("s-1") && !matches!(r.kind.as_str(), "exit" | "timeout")
        })
        .cloned()
        .collect();
    assert_eq!(fold(&unended, 0).human_wait_ms, 5_000);
    // NEGATIVE CONTROL: the overlap's first clear takes the entry down.
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

/// THE WORDS THE FOLD MATCHES ARE THE WRITERS': each line of [`WAITS`] that
/// carries one is built here from the journal module's consts, as its
/// writer builds it, and read back the same. NEGATIVE CONTROL: a reworded
/// line (a badge word, the skipped prefix, the retry's mail) is read as
/// something else.
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
    let retry = format!("attention=OK mail={MAIL_RETRY}, the episode of seq=60");
    assert_eq!(escalation_of(&retry), Escalation::Again);
    let adopted = format!("attention=OK mail={MAIL_ADOPTED}");
    assert_eq!(escalation_of(&adopted), Escalation::Again);
    // The turn-end policy's badge an Off park took down, set again as it
    // ends: no second escalation (the review of b512d03d8).
    let unparked = format!("attention=OK mail={MAIL_UNPARKED}");
    assert_eq!(escalation_of(&unparked), Escalation::Again);
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

/// The journals of [`a_stalls_hold_a_parks_start_and_a_loops_end_close_a_wait`]:
/// the loop's own lines, as `stall.rs`'s `hold_for_stall`, `lifecycle.rs`'s
/// `release_attention` and `park_while_off` write them, a session each.
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

/// A STALL'S HOLD, AN OFF PARK'S START AND A LOOP'S LAST CLEAR END A
/// PERSON'S WAIT ([`ENDS`]; the review of c1d193998): a `FROZEN` line whose
/// `attention=` reply took the loop's entry down closes the wait the badge
/// opened, with no `CLEARED` line after it; one whose reply is `kept` (a
/// limit's badge stands) or `skipped: …` (behind another supervisor) closes
/// nothing; the turn-end policy's `CLEARED` at the loop's end closes its
/// wait before the end line; and an Off park's `CLEARED` closes the wait
/// as the park starts, so the park is never a person's wait. NEGATIVE
/// CONTROLS: with the FROZEN line taken out, the loop's end drops the wait
/// uncounted; with the end's and the park's `CLEARED` lines taken out, the
/// end drops one wait and the park is counted as a person's wait; a
/// reworded FROZEN closes nothing.
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

    // NEGATIVE CONTROLS: the FROZEN line out, the end drops the wait.
    let unfrozen: Vec<JournalRecord> = mine("s-1")
        .into_iter()
        .filter(|r| !r.line.starts_with("FROZEN"))
        .collect();
    assert_eq!(fold(&unfrozen).human_wait_ms, 0);
    // The end's CLEARED out: dropped at the end.
    let uncleared: Vec<JournalRecord> = mine("s-4")
        .into_iter()
        .filter(|r| r.kind != "cleared")
        .collect();
    assert_eq!(fold(&uncleared).human_wait_ms, 0);
    // The park's CLEARED out: the park counted, 40 000 to 52 000.
    let parked: Vec<JournalRecord> = mine("s-5")
        .into_iter()
        .filter(|r| !(r.kind == "cleared" && r.t == 41_000))
        .collect();
    assert_eq!(fold(&parked).human_wait_ms, 12_000);
    // A reworded FROZEN.
    let reworded = journal(
        "
        1000 s-1 ESCALATED seq=10 attention=OK mail=OK
        3000 s-1 FROZE seq=11 input=stalled attention=OK mail=OK
        9000 s-1 CLEARED seq=12 box attention=OK
        ",
    );
    assert_eq!(fold(&reworded).human_wait_ms, 8_000);
}

/// THE FROZEN LINE THE FOLD READS IS THE WRITER'S: built here from the
/// journal module's consts as `hold_for_stall` builds it — the reply of the
/// unset, [`ATTENTION_KEPT`], or [`ATTENTION_SKIPPED`] behind another
/// supervisor — and read back the same. NEGATIVE CONTROL: another word
/// first, or no `attention=` field, takes nothing down.
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
    // NEGATIVE CONTROLS.
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

/// THE UPGRADE LEDGER: a `relaunched` row onto another build is an upgrade,
/// one on the build it ran (a crash relaunched, a restart in place) a
/// relaunch; the row's time is unix SECONDS. NEGATIVE CONTROLS: a build
/// unknown on either side is no evidence of a move; every other step
/// (`exited`, `adopted`, `wait:…`, a model row) counts nothing but rows; a
/// row before the window counts nothing.
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
    // Seconds against a window in ms: a window opening at 101 s keeps all
    // but the first row.
    let acc = fold(101_000);
    assert_eq!((acc.upgraded, acc.relaunched, acc.rows), (0, 3, 7));
    assert_eq!(fold(200_000).rows, 0);
}

/// A LINE THAT IS NO ROW is counted `bad`, in every fold, and never panics:
/// empty, cut short, not an object, no time, a time that is no integer.
/// Its neighbours still count. NEGATIVE CONTROL: the same row whole is no
/// `bad` line.
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
    // The journal's records are read before the fold (`stats_under` counts
    // a line that does not read as one): an empty slice folds nothing.
    fold_journal(&mut acc, &[]);
    assert_eq!(acc.rows, 1);
    // NEGATIVE CONTROL.
    let acc = fold_ledger(std::slice::from_ref(&good), 0);
    assert_eq!((acc.bad, acc.rows), (0, 1));
    // A fold with nothing in its window is empty.
    assert!(fold_ledger(&[good], 2_000).is_empty());
    assert!(HarnessStats::new(0).is_empty());
}

/// [`window_cut`]: a file long enough to have been cut whose oldest row is
/// newer than the window's start may have lost it. NEGATIVE CONTROLS: a file
/// shorter than its cut keeps was never cut; one whose oldest row is at or
/// before the start holds it whole; one with no row placed says nothing.
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

/// Every file and directory under `root`, recursively, sorted: what a reader
/// must leave as it found it.
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

/// A sweep's options over `harness_state`, in `upgrade_status_tests.rs`'
/// shape: the real writer of the upgrade ledger takes nothing else from them.
fn sweep_opts(home: &Path, harness_state: &Path) -> Opts {
    Opts {
        home: home.to_path_buf(),
        state: harness_state.to_path_buf(),
        sock: None,
        only_sid: None,
        dry_run: false,
        human_grace_s: 120,
        hand_back: true,
        background: false,
        aterm_state: None,
        ask: false,
        several_logins: false,
        usage_tap: None,
        instances: Instances::Listed(Vec::new()),
        agents_off: Vec::new(),
    }
}

/// One upgrade ledger row's report: `tab` moved `from` → `to`, `step` taken.
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

/// THE READER over a real state tree: two sessions' ledgers, one's journal,
/// and the upgrade ledger written by its own writer, each counted once; a
/// file that is no ledger (a goal-hold record, a cut's `.tmp`, a directory
/// named like a ledger) is passed over even where its text reads as rows; a
/// journal line that is no record is `bad`. NOTHING IS CREATED: the tree's
/// listing is the same after the read. A window after every row is empty.
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
    // Not ledgers, whatever their text says.
    let decoy = [row(at, "s-9", RULE_READ_ONLY, Outcome::Approved, "ls", "")];
    write("s-1.goal-hold.json", &decoy);
    write("s-1.jsonl.tmp", &decoy);
    std::fs::create_dir_all(drive.join("d.jsonl")).expect("a directory");
    // The upgrade ledger, through its own writer.
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
    // approved, unproven, declined, answered, continued, landed, resumed,
    // escalated, relaunched, upgraded.
    assert_eq!(counts(&acc), [1, 0, 1, 1, 1, 0, 1, 1, 1, 1], "{acc:#?}");
    assert_eq!((acc.human_wait_ms, acc.limit_wait_ms), (5_000, 9_000));
    assert_eq!(acc.sessions, 3, "s-1, s-2 and the upgrade's s-3");
    assert_eq!((acc.rows, acc.bad, acc.cut), (6 + 3 + 3, 1, false));
    assert_eq!(by(&acc, RULE_ACCOUNT), 1);
    assert_eq!(by(&acc, RULE_READ_ONLY), 1, "the decoys were not read");

    // A window that opens after every row.
    let later = stats_under(&state, &harness, at + 3_600_000);
    assert!(later.is_empty(), "{later:#?}");
    assert_eq!(listing(&root), before);
}

/// A MISSING STATE TREE is an empty result, and stays missing: the reader
/// makes no `drive` directory and no `upgrade` one. So is a tree whose
/// `drive` holds no ledger.
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

/// A LEDGER CUT BACK TO ITS KEPT ROWS whose oldest row is newer than the
/// window's start says the window is partial. NEGATIVE CONTROLS: one row
/// fewer than a cut keeps was never cut; a window that opens at its oldest
/// row is whole.
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

/// A LEDGER NO ROW WAS APPENDED TO SINCE THE WINDOW OPENED is not read: a
/// row's time is taken as it is appended, so none of its rows can be in the
/// window (the row planted here, newer than the file, is what shows it was
/// not read). NEGATIVE CONTROL: the same file written now is read.
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
    let set_mtime = |at: std::time::SystemTime| {
        std::fs::File::open(&path)
            .and_then(|f| f.set_modified(at))
            .expect("set mtime");
    };
    // Written a day before the window opened.
    let day_before = u64::try_from(since_ms - 86_400_000).expect("after the epoch");
    set_mtime(std::time::UNIX_EPOCH + std::time::Duration::from_millis(day_before));
    assert!(stats_under(&state, &harness, since_ms).is_empty());
    // NEGATIVE CONTROL: written now.
    set_mtime(std::time::SystemTime::now());
    assert_eq!(stats_under(&state, &harness, since_ms).continued, 1);
}
