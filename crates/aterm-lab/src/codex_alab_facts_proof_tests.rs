// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const HEX: &str = "00c0ffee0123abcd";

fn journal(lines: &[&str]) -> String {
    lines
        .iter()
        .map(|l| format!("2026-11-04T21:00:00.000000Z DEBUG codex_alab_autopilot: {l}\n"))
        .collect()
}

/// CODEX-ALAB'S FACT LINES READ AS IT JOURNALS THEM (its `Supervisor::fact_for`, `fact_told`
/// and `submit`, a6430a662d): a read's `why`, a ride and its ack in order, any other fact line
/// kept apart, and no other journal line. NEGATIVE CONTROLS: an episode of 15 digits or in
/// capitals, a rule that is no rule id, and the no-terminal line (`<key>: fact: …`) are no
/// read, ride or ack.
#[test]
fn codex_alab_s_fact_lines_tell_the_reads_the_ride_and_its_ack() {
    let log = journal(&[
        "fact: none (continue@v1)",
        "fact: stale (continue@v1)",
        "t1: act continue@v1: typed",
        &format!("t1: fact no-progress {HEX} rides continue@v1"),
        &format!("fact no-progress {HEX} told"),
        "fact: told (continue@v1)",
        "fact: ERR denied (continue@v1)",
        "t1: fact: no background terminal beside another root thread (continue@v1)",
        "fact: aterm 0.166.0 has no harness fact (ERR usage: harness policy | recovery); not asked again under this claim",
        &format!(
            "t1: fact no-progress {HEX} gone stale before the start: continue@v1 typed without it"
        ),
        &format!("fact no-progress {HEX} not told: Refused(\"ERR stale fact\")"),
        "fact: none (not a rule)",
        &format!("t1: fact no-progress {} rides continue@v1", &HEX[1..]),
        &format!(
            "t1: fact no-progress {} rides continue@v1",
            HEX.to_uppercase()
        ),
    ]);
    let lines = fact_lines(&log);
    let s = story(&lines);
    let at = crate::workers::iso_ms("2026-11-04T21:00:00.000Z");
    assert!(at.is_some());
    assert_eq!(
        s.before,
        [("none".to_string(), at), ("stale".to_string(), at)]
    );
    assert_eq!(s.rides, [(HEX.to_string(), "continue@v1".to_string())]);
    assert_eq!(s.told, [HEX]);
    assert_eq!(s.after, ["told", "ERR denied"]);
    assert_eq!(s.other.len(), 7, "{:?}", s.other);
    assert!(fact_lines(&journal(&["t1: act continue@v1: typed"])).is_empty());
}

/// Journal lines as codex-alab writes them, each at its own UTC stamp.
fn stamped(lines: &[(&str, &str)]) -> String {
    lines
        .iter()
        .map(|(at, l)| format!("{at} DEBUG codex_alab_autopilot: {l}\n"))
        .collect()
}

/// CHECK 1 IN TIME, OVER RECORDED ROWS. L3's (`docs/measured/codex-alab-facts-2026-11-04.md`:
/// a `none` 116 s into the stall, then the ride at 246 s) hold. The late ride's do not: a
/// second act read `none` at 246 s, past φ 8's 184 s plus the board's 5 s and 3 s of slack,
/// and only the third carried the fact. NEGATIVE CONTROLS: a late read that answered `stale`
/// holds; a first read already past φ 8 (no read before the stall could be named), an
/// unstamped read, an answer other than `none` or `stale`, and no read at all do not.
#[test]
fn check_1_holds_l3_s_reads_and_refuses_a_late_ride() {
    let blocked = crate::workers::iso_ms("2026-11-04T22:22:52.500Z").expect("a stamp");
    let reads = |rows: &[(&str, &str)]| story(&fact_lines(&stamped(rows))).before;
    let none = "fact: none (continue@v1)";
    let stale = "fact: stale (continue@v1)";
    let rides =
        "01a1a8a8-2b04-7e42-9ab1-95d63e47f9f6: fact no-progress e51ba952cba02aca rides continue@v1";
    let (first, second, third) = (
        "2026-11-04T22:24:48.194454Z",
        "2026-11-04T22:26:58.219119Z",
        "2026-11-04T22:29:08.251521Z",
    );
    let l3 = reads(&[
        (first, none),
        (second, rides),
        (
            "2026-11-04T22:26:58.225327Z",
            "fact no-progress e51ba952cba02aca told",
        ),
        (third, "fact: told (continue@v1)"),
    ]);
    assert_eq!(reads_at(&l3, blocked), ["none at 115.7 s"]);
    assert!(reads_before_hold(&l3, blocked));
    let late = reads(&[(first, none), (second, none), (third, rides)]);
    assert_eq!(
        reads_at(&late, blocked),
        ["none at 115.7 s", "none at 245.7 s"]
    );
    assert!(!reads_before_hold(&late, blocked));
    assert!(reads_before_hold(
        &reads(&[(first, none), (second, stale), (third, rides)]),
        blocked
    ));
    assert!(!reads_before_hold(
        &reads(&[(second, stale), (third, rides)]),
        blocked
    ));
    let unstamped = reads(&[("-", none), (third, rides)]);
    assert_eq!(unstamped, [("none".to_string(), None)]);
    assert!(!reads_before_hold(&unstamped, blocked));
    assert!(!reads_before_hold(
        &reads(&[(first, "fact: busy (continue@v1)"), (third, rides)]),
        blocked
    ));
    assert!(!reads_before_hold(&reads(&[(third, rides)]), blocked));
}

/// THE RIDER IS ATERM'S OWN WORDS: the rider at the end of the act, the pid and minutes it
/// names, and aterm's `fact_suffix` of `BackgroundCall::told` for them agree. NEGATIVE
/// CONTROLS: a rider whose frame, pid or minutes differ is not the expected one, and words
/// with no rider have none.
#[test]
fn the_expected_rider_is_aterms_fact_suffix_for_the_call() {
    let rider = " (aterm: a background call (pid 4200) has made no progress for 4 min)";
    let act = format!("keep going (standing rules: be brief){rider}");
    assert_eq!(rider_of(&act), Some(rider));
    assert_eq!(told_call(rider), Some((4200, 4)));
    assert_eq!(expected_rider(4200, 4), rider);
    assert_ne!(expected_rider(4201, 4), rider);
    assert_ne!(expected_rider(4200, 5), rider);
    assert_ne!(
        expected_rider(4200, 4),
        rider.replace(" (aterm: ", " (aterm said: ")
    );
    assert_eq!(rider_of("keep going (standing rules: be brief)"), None);
    assert_eq!(
        told_call(" (aterm: a background call has made no progress)"),
        None
    );
}

/// `sys jobs` names the agent calls it reads `no_progress=1` by their leader. NEGATIVE
/// CONTROLS: a quiet call without the word, a shell job and a finding are none.
#[test]
fn sys_jobs_names_the_no_progress_calls_by_their_leader() {
    let reply = "OK 4 sys v=1 view=jobs as_of=1 age_ms=10 window_ms=0 next=1.2\n\
         job id=a4200 kind=agent-call consumer=agent pgid=4200 exe=perl class=waiting wait=unnamed quiet_ms=190000 phi=8.25 mu_ms=10000 mu_from=own verdict=quiet no_progress=1\n\
         job id=a4100 kind=agent-call consumer=agent pgid=4100 exe=perl class=stopped stop=held stopped_pid=4100:perl quiet_ms=220000 phi=1.59 verdict=fine\n\
         job id=b5 kind=shell consumer=person-fg exe=codex class=running verdict=fine\n\
         finding kind=stopped pid=4100 exe=perl\n";
    assert_eq!(no_progress_leaders(reply), [4200]);
    assert!(no_progress_leaders(&reply.replace(" no_progress=1", "")).is_empty());
}

/// The ledger's fact rows are read with their holder, the sid without its `@`. NEGATIVE
/// CONTROL: a self-claim's act row is no fact row.
#[test]
fn the_ledger_s_fact_rows_carry_their_holder() {
    let rows = [
        r#"{"ts":1,"sid":"@s-1","rule_id":"fact-no-progress@v1","decision":"typed","command":"a background call (pid 4200) has made no progress for 4 min","reason":"kind=no-progress episode=00c0ffee0123abcd door=turn-end","holder":"codex-alab@77"}"#,
        r#"{"ts":2,"sid":"s-1","rule_id":"self-claim-act@v1","decision":"typed","command":"","holder":"codex-alab@77"}"#,
    ]
    .map(|r| J::parse(r).expect("a row"));
    assert_eq!(
        ledger_facts(&rows),
        [LedgerFact {
            sid: "s-1".to_string(),
            decision: "typed".to_string(),
            command: "a background call (pid 4200) has made no progress for 4 min".to_string(),
            reason: format!("kind=no-progress episode={HEX} door=turn-end"),
            holder: "codex-alab@77".to_string(),
        }]
    );
}

/// Messages rows are read by their decoded title and detail, the detail's lines rejoined, so a
/// rider split across two of them reads whole. NEGATIVE CONTROL: the header is no row.
#[test]
fn messages_rows_are_read_by_their_decoded_words() {
    let reply = "OK 1 messages\n\
         message 7 at=1 ago_ms=2 tag=harness sev=info origin=wire state=live rep=1 key=- title=codex-alab%20%405 detail=typed%20%22keep%20going%20(ate%0Arm%3A%20x)%22 actions=-\n";
    assert_eq!(
        message_texts(reply),
        ["codex-alab @5 typed \"keep going (aterm: x)\""]
    );
}
