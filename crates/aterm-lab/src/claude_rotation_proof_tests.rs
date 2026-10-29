// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The rotation proof's readers and judges, on excerpts of real runs (lab `rp1`, Claude Code
//! 2.1.310, 2026-10-26; labs `rp2`, `rp3` and `rp6`, Claude Code 2.1.311, 2026-10-27): each
//! judge passes on the run's own text and fails, naming the clause, when one fact of it is taken
//! away or turned.

use super::*;

/// The local midnight the excerpts' stub stamps count from: 2026-10-26 00:00 PDT.
const DAY: i64 = 1_792_998_000_000;
/// The tab's sid.
const SID: &str = "s-9005e7a0ed79d1417b86";

/// The stub's log, cut to the run's `/v1/messages` lines (their `RL200[…]` tails cut too).
const STUB_LOG: &str = r#"02:02:14.637 stub listening on 127.0.0.1:63213 (proxy black hole: 63214) mode=ok pid=64807
02:02:18.066 MODE -> ok (from file)
02:02:18.067 POST /v1/messages -> 200 body=4069B mode=ok kind=aux model=claude-opus-5-5 stream=true max_tokens=128000 msgs=1 tools=0 sys=3188c last_user="<session> Write one sentence about lighthouses, then stop. </session>~" auth=oat:lab-a RL200[5h-utilization=0.10]
02:02:18.067 POST /v1/messages -> 200 body=53190B mode=ok kind=main model=claude-opus-5-5 stream=true max_tokens=128000 msgs=2 tools=13 sys=6082c last_user="<system-reminder> Attribution for git commits and pull requests you c~" auth=oat:lab-a RL200[5h-utilization=0.10]
02:02:18.961 POST /v1/messages -> 429 body=52031B mode=limit:five_hour:1793005758 kind=main model=claude-opus-5-5 stream=true max_tokens=128000 msgs=5 tools=13 sys=6082c last_user="Write one sentence about harbours, then stop." auth=oat:lab-a RL200[5h-utilization=0.10]
02:02:52.802 POST /v1/messages -> 200 body=53927B mode=ok kind=main model=claude-opus-5-5 stream=true max_tokens=128000 msgs=5 tools=13 sys=6082c last_user="Write one sentence about harbours, then stop. [aterm harness] Autopil~" auth=oat:lab-b RL200[5h-utilization=0.10]
02:02:53.672 POST /v1/messages -> 429 body=52765B mode=limit:five_hour:1793007773 kind=main model=claude-opus-5-5 stream=true max_tokens=128000 msgs=8 tools=13 sys=6082c last_user="Write one sentence about tides, then stop." auth=oat:lab-b RL200[5h-utilization=0.10]
"#;

/// The tab's journal around R1's move, as the instance wrote it.
const JOURNAL_R1: &str = r#"{"t":1793005339364,"sid":"s-9005e7a0ed79d1417b86","kind":"event","phase":"idle","seq":613,"line":"EVENT idle seq=613 ⏺ Usage limit reached · continuing automatically at 2:09am · esc to cancel"}
{"t":1793005339365,"sid":"s-9005e7a0ed79d1417b86","kind":"skipped","phase":"wall-confirm@v1","seq":613,"line":"SKIPPED seq=613 rule=wall-confirm@v1 unconfirmed wall: usage-session — the transcript ends on the model's reply: ⚠ Usage limit reached · limit resets 2:09am — Continuing automatically at 2:09am · esc to cancel"}
{"t":1793005339365,"sid":"s-9005e7a0ed79d1417b86","kind":"waiting","phase":"-","seq":613,"line":"WAITING seq=613 until=2026-10-26T09:04:19Z 1 short turn(s) in a row: 2 min back-off"}
{"t":1793005339873,"sid":"s-9005e7a0ed79d1417b86","kind":"limited","phase":"-","seq":619,"line":"LIMITED seq=619 handled: ⚠ Usage limit reached · limit resets 2:09am — Continuing automatically at 2:09am · esc to cancel"}
{"t":1793005356672,"sid":"s-9005e7a0ed79d1417b86","kind":"other","phase":"-","seq":null,"line":"HOST seq=619 restart:account step=adopted"}
{"t":1793005356683,"sid":"s-9005e7a0ed79d1417b86","kind":"cleared","phase":"-","seq":619,"line":"CLEARED seq=619 attention=none relaunched under another account, off the limited one"}
{"t":1793005356683,"sid":"s-9005e7a0ed79d1417b86","kind":"restarted","phase":"account-rotate@v1","seq":619,"line":"RESTARTED seq=619 rule=account-rotate@v1 relaunch under account lab-b (from the limit its turn ended on; the same conversation)"}
{"t":1793005371714,"sid":"s-9005e7a0ed79d1417b86","kind":"other","phase":"-","seq":null,"line":"HOST seq=619 carry-on step=done:resumed"}
{"t":1793005372745,"sid":"s-9005e7a0ed79d1417b86","kind":"continued","phase":"usage-resume@v1","seq":394,"line":"CONTINUED seq=394 rule=usage-resume@v1 [aterm harness] Autopilot is Cruise: keep going. Finish everything in hand, verify it and land it"}
"#;

/// R3's records: the wait on `lab-b` until `lab-a` opens.
const JOURNAL_R3: &str = r#"{"t":1793005374266,"sid":"s-9005e7a0ed79d1417b86","kind":"waiting","phase":"-","seq":1065,"line":"WAITING seq=1065 until=2026-10-26T09:03:24Z the worker to take the usage-resume@v1 act"}
{"t":1793005404332,"sid":"s-9005e7a0ed79d1417b86","kind":"waiting","phase":"-","seq":1065,"line":"WAITING seq=1065 until=2026-10-26T09:11:48Z another account can be used then"}
"#;

/// The accounts' ledger through R3's wall.
const LEDGER: &str = r#"{"account":"lab-a","kind":"sample","sid":"s-9005e7a0ed79d1417b86","t":1793005338,"to":null,"why":"tap: session 10% week 20%"}
{"account":"lab-a","kind":"wall","sid":"s-9005e7a0ed79d1417b86","t":1793005338,"to":null,"why":"five_hour until 1793005758"}
{"account":"lab-a","kind":"rotate","sid":"s-9005e7a0ed79d1417b86","t":1793005339,"to":"lab-b","why":""}
{"account":"lab-a","kind":"move","sid":"s-9005e7a0ed79d1417b86","t":1793005356,"to":"lab-b","why":""}
{"account":"lab-b","kind":"proven","sid":"s-9005e7a0ed79d1417b86","t":1793005374,"to":null,"why":""}
{"account":"lab-b","kind":"struck","sid":"s-9005e7a0ed79d1417b86","t":1793005404,"to":null,"why":"wall: five_hour"}
{"account":"lab-b","kind":"wall","sid":"s-9005e7a0ed79d1417b86","t":1793005404,"to":null,"why":"five_hour until 1793007773"}
"#;

/// The instance's aterm.log, its harness lines for R1 and R3's wall.
const ATERM_LOG: &str = "\
1793005339.363 64997 INFO aterm_gui::harness_host: harness @s-9005e7a0ed79d1417b86: account advice: stay (nothing-due)
1793005339.872 64997 INFO aterm_gui::harness_host: harness @s-9005e7a0ed79d1417b86: account advice: move to lab-b
1793005339.941 64997 INFO aterm_gui::harness_host: harness @s-9005e7a0ed79d1417b86: account wall wall lab-a
1793005356.536 64997 INFO aterm_gui::harness_host: harness @s-9005e7a0ed79d1417b86: restart:account pid=65182 tab=s-9005e7a0ed79d1417b86 session=788f0632-b0bd-4f60-b186-50e8de5b0ca7 from=2.1.310 to=2.1.310(same) step=adopted
1793005356.672 64997 INFO aterm_gui::harness_host: harness @s-9005e7a0ed79d1417b86: account move to lab-b: adopted
1793005372.229 64997 INFO aterm_gui::harness_host: harness @s-9005e7a0ed79d1417b86: account wall not-this-process lab-b
1793005374.263 64997 INFO aterm_gui::harness_host: harness @s-9005e7a0ed79d1417b86: account advice: wait on lab-b (until 1793005908)
1793005404.329 64997 INFO aterm_gui::harness_host: harness @s-9005e7a0ed79d1417b86: account lab-b struck: wall
";

/// The upgrade ledger's restart steps: R1's `restart:account`, and its `adopted` row of the
/// process aterm.log names (written in the shape lab `rp6`'s ledger has it).
const DRIVE_LEDGER: &str = r#"{"detail":"SIGTERM","from":"2.1.310","pid":65070,"session":"788f0632-b0bd-4f60-b186-50e8de5b0ca7","step":"restart:account","t":1793005340,"tab":"s-9005e7a0ed79d1417b86","to":"2.1.310(same)"}
{"detail":"the relaunched agent holds the conversation; resumed while idle, nothing typed","from":"2.1.310","pid":65182,"session":"788f0632-b0bd-4f60-b186-50e8de5b0ca7","step":"adopted","t":1793005356,"tab":"s-9005e7a0ed79d1417b86","to":"2.1.310(same)"}
"#;

/// Where the run's two logins lived.
const DIR_A: &str = "/private/tmp/at.lab-501/aterm-8dd99024/runs/rp1/scratch/home/.claude";
const DIR_B: &str = "/private/tmp/at.lab-501/aterm-8dd99024/runs/rp1/scratch/state/harness/claude-harness/accounts/logins/lab-b";

fn dirs() -> Vec<(String, String)> {
    vec![
        (LABEL_A.into(), DIR_A.into()),
        (LABEL_B.into(), DIR_B.into()),
    ]
}

/// R1's evidence: the excerpts, the processes the watcher saw and the moved-to turn's history.
fn r1() -> (Ev, Leg) {
    let ev = Ev {
        journal: journal_of(JOURNAL_R1),
        ledger: rows_of(LEDGER),
        aterm_log: ATERM_LOG.into(),
        served: served_of(STUB_LOG, DAY),
        launches: launches_of(&format!(
            "1793005336000 65070 {DIR_A} fresh\n1793005356400 65182 {DIR_B} resume\n"
        )),
        drive_ledger: DRIVE_LEDGER.into(),
        dirs: dirs(),
        sid: SID.into(),
    };
    let leg = Leg {
        from: 1_793_005_338_100,
        to: 1_793_005_373_000,
        reset: 1_793_005_758,
        history: vec![
            P1.into(),
            format!("{P2}\n\n[aterm harness] Autopilot is Cruise: keep going."),
        ],
        ..Leg::default()
    };
    (ev, leg)
}

#[test]
fn a_stub_stamp_reads_as_ms_into_its_day_and_nothing_else_does() {
    assert_eq!(stamp_ms("00:00:00.000"), Some(0));
    assert_eq!(
        stamp_ms("13:02:03.045"),
        Some(((13 * 60 + 2) * 60 + 3) * 1000 + 45)
    );
    for bad in [
        "24:00:00.000",
        "12:60:00.000",
        "12:00:00",
        "12:00:00.1000",
        "x",
    ] {
        assert_eq!(stamp_ms(bad), None, "{bad}");
    }
}

#[test]
fn the_stub_log_reads_as_its_answers_each_with_its_credential() {
    let served = served_of(STUB_LOG, DAY);
    assert_eq!(served.len(), 5, "{served:#?}");
    let wall = &served[2];
    assert_eq!(
        (wall.status, wall.kind.as_str(), wall.auth.as_str()),
        (429, "main", AUTH_A)
    );
    assert_eq!(wall.mode, "limit:five_hour:1793005758");
    assert_eq!(wall.last_user, P2);
    assert_eq!(wall.t_ms, DAY + ((2 * 60 + 2) * 60 + 18) * 1000 + 961);
    assert_eq!(served[3].auth, AUTH_B);
    assert!(
        served[3].last_user.starts_with(P2),
        "{}",
        served[3].last_user
    );
    // A stamp far before the one before it is the next day's.
    let rolled = served_of(
        "23:59:59.000 POST /v1/messages -> 200 mode=ok kind=main last_user=\"a\" auth=oat:lab-a\n\
         00:00:01.000 POST /v1/messages -> 200 mode=ok kind=main last_user=\"b\" auth=oat:lab-b\n",
        DAY,
    );
    assert_eq!(rolled[1].t_ms - rolled[0].t_ms, 2000);
    assert_eq!(rolled[1].last_user, "b");
}

#[test]
fn the_ledger_journal_and_log_read_as_the_judges_need_them() {
    let rows = rows_of(LEDGER);
    assert_eq!(rows.len(), 7);
    assert_eq!(
        (rows[2].kind.as_str(), rows[2].to.as_str(), rows[2].t),
        ("rotate", LABEL_B, 1_793_005_339)
    );
    assert_eq!(
        rows[3].to, LABEL_B,
        "a null `to` reads as empty, a label as itself"
    );
    assert_eq!(rows[0].to, "");
    let journal = journal_of(JOURNAL_R1);
    let moved = restarted(&journal, LABEL_B, (0, i64::MAX)).expect("the move");
    assert_eq!(moved.t_ms, 1_793_005_356_683);
    assert!(restarted(&journal, LABEL_A, (0, i64::MAX)).is_none());
    assert!(restarted(&journal, LABEL_B, (0, 1_793_005_356_000)).is_none());
    assert!(
        journal
            .iter()
            .any(|s| typed_for(&s.line, RULE_LIMIT_RESUME))
    );
    assert!(!journal.iter().any(|s| typed_for(&s.line, RULE_ACCOUNT)));
    assert!(!typed_for(
        "TYPEDX seq=1 rule=usage-resume@v1",
        RULE_LIMIT_RESUME
    ));
    let r3 = journal_of(JOURNAL_R3);
    assert_eq!(waiting_until(&r3[1].line), Some(1_793_005_908_000));
    assert_eq!(
        advice_until(ATERM_LOG, LABEL_B, 1_793_005_374_000),
        Some(1_793_005_908)
    );
    assert_eq!(
        advice_until(ATERM_LOG, LABEL_B, 1_793_005_375_000),
        None,
        "an advice before the window is not this one's"
    );
    assert_eq!(advice_until(ATERM_LOG, LABEL_A, 0), None);
}

#[test]
fn a_launch_line_reads_back_as_itself() {
    let l = Launched {
        t_ms: 7,
        pid: 65182,
        login: DIR_B.into(),
        resumed: true,
    };
    assert_eq!(launches_of(&launch_line(&l)), vec![l.clone()]);
    let fresh = Launched {
        resumed: false,
        login: "-".into(),
        ..l
    };
    assert_eq!(launches_of(&launch_line(&fresh)), vec![fresh]);
    assert!(launches_of("x 1 - fresh\n7\n").is_empty());
}

#[test]
fn each_live_login_carries_a_token_the_stub_reads_as_its_label_alone() {
    let login = J::parse(live_login(LABEL_B, 1_000).trim()).expect("JSON");
    let oauth = login.get("claudeAiOauth").expect("claudeAiOauth");
    let token = oauth.get("accessToken").and_then(J::as_str).expect("token");
    assert_eq!(
        crate::stub::auth_label(Some(&format!("Bearer {token}"))),
        AUTH_B
    );
    assert_eq!(
        oauth.get("expiresAt").and_then(J::as_i64),
        Some(1_000 + 365 * 86_400_000)
    );
    assert_eq!(
        crate::stub::auth_label(Some(&format!(
            "Bearer {}",
            J::parse(live_login(LABEL_A, 0).trim())
                .ok()
                .and_then(|j| j
                    .get("claudeAiOauth")?
                    .get("accessToken")?
                    .as_str()
                    .map(str::to_string))
                .unwrap_or_default()
        ))),
        AUTH_A
    );
}

#[test]
fn r1_passes_on_the_runs_own_move_and_fails_on_each_fact_taken_away() {
    let (ev, leg) = r1();
    let held = judge_r1(&ev, &leg);
    assert_eq!(held.verdict, Verdict::Ok, "{}", held.evidence);
    let fails = |ev: &Ev, leg: &Leg, says: &str| {
        let j = judge_r1(ev, leg);
        assert_eq!(j.verdict, Verdict::Fail, "{says}: {}", j.evidence);
        assert!(j.evidence.contains(says), "{says}: {}", j.evidence);
    };
    // No move in the journal.
    let mut no_move = ev.clone();
    no_move
        .journal
        .retain(|s| !s.line.starts_with("RESTARTED "));
    fails(&no_move, &leg, "no RESTARTED account-rotate@v1 onto lab-b");
    // A move at a turn end is no move from the wall.
    let mut ahead = ev.clone();
    for s in &mut ahead.journal {
        s.line = s.line.replace(
            "(from the limit its turn ended on; the same conversation)",
            "(at a turn end; the same conversation)",
        );
    }
    fails(&ahead, &leg, "the restart is not from the limit");
    // The next turn served under the limited row's credential.
    let mut stayed = ev.clone();
    for s in &mut stayed.served {
        if s.auth == AUTH_B {
            s.auth = AUTH_A.into();
        }
    }
    fails(&stayed, &leg, "carried oat:lab-a");
    // The conversation lost: the moved-to row's turn without the first prompt.
    let mut fresh = leg.clone();
    fresh.history.remove(0);
    fails(
        &ev,
        &fresh,
        "the history lacks `Write one sentence about lighthouses",
    );
    // The process the restart adopted: never seen launched, launched on another login, or none
    // named.
    let mut nobody = ev.clone();
    nobody.launches.retain(|l| l.login != DIR_B);
    fails(
        &nobody,
        &leg,
        "the adopted process (pid 65182) was never seen launched",
    );
    let mut elsewhere = ev.clone();
    for l in &mut elsewhere.launches {
        l.login = DIR_A.into();
    }
    fails(&elsewhere, &leg, "runs on login");
    let mut unnamed = ev.clone();
    unnamed.aterm_log = unnamed.aterm_log.replace(" step=adopted", " step=refused");
    fails(&unnamed, &leg, "names no process a restart:account adopted");
    // The ledger without the rotation, or without lab-b proven.
    let mut unbooked = ev.clone();
    unbooked.ledger.retain(|r| r.kind != "rotate");
    fails(&unbooked, &leg, "no `rotate` row lab-a → lab-b");
    let mut unproven = ev.clone();
    unproven.ledger.retain(|r| r.kind != "proven");
    fails(&unproven, &leg, "no `proven` row for lab-b");
    // No resume typed after the restart.
    let mut silent = ev.clone();
    silent.journal.retain(|s| !s.line.starts_with("CONTINUED "));
    fails(&silent, &leg, "no usage-resume@v1 act after the restart");
    // The upgrade ledger: a move ahead's step is no wall move's, a step of another tab or from
    // before the window is not this move's, and one whose adopted process aterm.log does not name
    // is not either.
    let no_step = "the upgrade ledger has no restart:account step of the tab in the window";
    for (from, to) in [
        ("\"restart:account\"", "\"restart:account-ahead\""),
        (
            "\"tab\":\"s-9005e7a0ed79d1417b86\"",
            "\"tab\":\"s-0000000000000000000a\"",
        ),
        ("\"t\":1793005340", "\"t\":1793005300"),
    ] {
        let mut other = ev.clone();
        other.drive_ledger = DRIVE_LEDGER.replacen(from, to, 1);
        fails(&other, &leg, no_step);
    }
    let mut stranger = ev.clone();
    stranger.drive_ledger = DRIVE_LEDGER.replace("\"pid\":65182", "\"pid\":65999");
    fails(&stranger, &leg, "adopted pid 65999");
    let mut unadopted = ev.clone();
    unadopted.drive_ledger = DRIVE_LEDGER.lines().take(1).collect();
    fails(&unadopted, &leg, "followed by no `adopted` row of the tab");
    // The host's own advice missing.
    let mut unadvised = ev;
    unadvised.aterm_log = unadvised.aterm_log.replace("move to lab-b", "stay (x)");
    fails(&unadvised, &leg, "no `account advice: move to lab-b`");
}

#[test]
fn r3_waits_for_the_first_row_to_open_and_fails_on_an_early_move_or_another_instant() {
    let (mut ev, _) = r1();
    ev.journal.extend(journal_of(JOURNAL_R3));
    // The move back as R3's half of the run made it (the lines the host writes for R1's).
    let back = 1_793_005_910_000;
    ev.journal.extend([
        Said {
            t_ms: back,
            line: "RESTARTED seq=1200 rule=account-rotate@v1 relaunch under account lab-a (from the limit its turn ended on; the same conversation)".into(),
        },
        Said {
            t_ms: back + 16_000,
            line: "CONTINUED seq=1201 rule=usage-resume@v1 [aterm harness] Autopilot is Cruise: keep going.".into(),
        },
    ]);
    ev.aterm_log.push_str(&format!(
        "{}.000 1 INFO x: harness @{SID}: account advice: move to lab-a\n\
         {}.000 1 INFO x: harness @{SID}: restart:account pid=66000 tab={SID} step=adopted\n",
        back / 1000 - 1,
        back / 1000
    ));
    ev.ledger.extend(rows_of(
        "{\"account\":\"lab-b\",\"kind\":\"rotate\",\"t\":1793005909,\"to\":\"lab-a\",\"why\":\"\"}\n\
         {\"account\":\"lab-a\",\"kind\":\"proven\",\"t\":1793005930,\"to\":null,\"why\":\"\"}\n",
    ));
    let r1_steps = ev.drive_ledger.clone();
    ev.drive_ledger.push_str(&format!(
        "{{\"pid\":65182,\"step\":\"restart:account\",\"t\":{},\"tab\":\"{SID}\"}}\n\
         {{\"pid\":66000,\"step\":\"adopted\",\"t\":{},\"tab\":\"{SID}\"}}\n",
        back / 1000 - 1,
        back / 1000
    ));
    ev.launches.push(Launched {
        t_ms: back + 300,
        pid: 66000,
        login: DIR_A.into(),
        resumed: true,
    });
    ev.served.push(Served {
        t_ms: back + 17_000,
        status: 200,
        kind: "main".into(),
        auth: AUTH_A.into(),
        mode: "ok".into(),
        last_user: P3.into(),
    });
    let leg = Leg {
        from: 1_793_005_373_100,
        to: back + 60_000,
        reset: 1_793_007_773,
        history: vec![P1.into(), P2.into(), P3.into()],
        ..Leg::default()
    };
    let a_reset = 1_793_005_758;
    let held = judge_r3(&ev, &leg, a_reset);
    assert_eq!(held.verdict, Verdict::Ok, "{}", held.evidence);
    // The upgrade ledger holds R1's step alone: R3's own move booked none, which R1's row, out of
    // R3's window, does not stand for.
    let mut r1_only = ev.clone();
    r1_only.drive_ledger = r1_steps;
    let j = judge_r3(&r1_only, &leg, a_reset);
    assert_eq!(j.verdict, Verdict::Fail, "{}", j.evidence);
    assert!(
        j.evidence
            .contains("the upgrade ledger has no restart:account step of the tab in the window"),
        "{}",
        j.evidence
    );
    // Moved back before the first row opened.
    let mut early = ev.clone();
    for s in &mut early.journal {
        if s.t_ms == back {
            s.t_ms = 1_793_005_600_000;
        }
    }
    let j = judge_r3(&early, &leg, a_reset);
    assert_eq!(j.verdict, Verdict::Fail, "{}", j.evidence);
    assert!(j.evidence.contains("before lab-a opened"), "{}", j.evidence);
    // A wait for an instant that is not the first row's opening.
    let mut other = ev.clone();
    other.aterm_log = other
        .aterm_log
        .replace("(until 1793005908)", "(until 1793007773)");
    let j = judge_r3(&other, &leg, a_reset);
    assert_eq!(j.verdict, Verdict::Fail, "{}", j.evidence);
    assert!(j.evidence.contains("not lab-a's opening"), "{}", j.evidence);
    // No WAITING record naming it: a wait nobody can see.
    let mut unsaid = ev;
    unsaid
        .journal
        .retain(|s| !s.line.contains("another account can be used then"));
    let j = judge_r3(&unsaid, &leg, a_reset);
    assert_eq!(j.verdict, Verdict::Fail, "{}", j.evidence);
    assert!(j.evidence.contains("no WAITING record"), "{}", j.evidence);
}

#[test]
fn the_stub_row_holds_only_where_every_request_carried_a_roster_rows_credential() {
    let mut served = served_of(STUB_LOG, DAY);
    // rp1 stopped in R3, before lab-c's login was made: no lab-c turn, so unproven.
    assert_eq!(judge_stub(&served).verdict, Verdict::Unproven);
    let mut on_c = served[3].clone();
    on_c.auth = AUTH_C.into();
    served.push(on_c);
    assert_eq!(judge_stub(&served).verdict, Verdict::Ok);
    let only_a: Vec<Served> = served
        .iter()
        .filter(|s| s.auth == AUTH_A)
        .cloned()
        .collect();
    assert_eq!(judge_stub(&only_a).verdict, Verdict::Unproven);
    let mut other = served;
    other[1].auth = "none".into();
    let j = judge_stub(&other);
    assert_eq!(j.verdict, Verdict::Fail);
    assert!(j.evidence.contains("auth=none"), "{}", j.evidence);
}

/// Lab `rp6`'s whole `messages.log` (Claude Code 2.1.311, 2026-10-27): the instance's install
/// facts at its start, the lab's own autopilot record, and the no-route error the sync raised for
/// `lab-c` (no login until R2) 991 ms before the first prompt, folded into twelve times through
/// R1 to R5b and resolved once R2 made `lab-c`'s login.
const MESSAGES_RP6: &str = "\
m1\tkind=posted\tid=1\tt=1793129279309\ttag=harness\tsev=warn\tglyph=⚠\ttitle=Claude Code is not installed\tdetail=Claude Code is wanted on this machine, and installing it is your choice\\uInstall Claude Code in Settings\\unot installed\\uIn a terminal, aterm ai doctor signs in every AI CLI that needs it\tactions=open-settings:/ai\tkey=ai.claude.not-installed\torigin=host\tneed=later\tend=\n\
m1\tkind=posted\tid=2\tt=1793129279309\ttag=harness\tsev=info\tglyph=ℹ\ttitle=Codex CLI is not installed\tdetail=Codex CLI is wanted on this machine, and installing it is your choice\\uInstall Codex CLI in Settings\\unot installed\\uIn a terminal, aterm ai doctor signs in every AI CLI that needs it\tactions=open-settings:/ai\tkey=ai.codex.not-installed\torigin=host\tneed=later\tend=\n\
m1\tkind=posted\tid=3\tt=1793129279309\ttag=harness\tsev=info\tglyph=ℹ\ttitle=Antigravity CLI is not installed\tdetail=Antigravity CLI is wanted on this machine, and installing it is your choice\\uInstall Antigravity CLI in Settings\\unot installed (not measured in this build)\\uIn a terminal, aterm ai doctor signs in every AI CLI that needs it\tactions=open-settings:/ai\tkey=ai.agy.not-installed\torigin=host\tneed=later\tend=\n\
m1\tkind=posted\tid=4\tt=1793129279309\ttag=harness\tsev=info\tglyph=ℹ\ttitle=Muse Code is not installed\tdetail=Muse Code is wanted on this machine, and installing it is your choice\\uInstall Muse Code in Settings\\unot installed (not measured in this build)\\uIn a terminal, aterm ai doctor signs in every AI CLI that needs it\tactions=open-settings:/ai\tkey=ai.muse.not-installed\torigin=host\tneed=later\tend=\n\
m1\tkind=posted\tid=5\tt=1793129339531\ttag=harness\tsev=info\tglyph=ℹ\ttitle=Autopilot set to cruise\tdetail=Tab 1: the harness keeps going for ever, finding more to do\\uthis session's own setting\\uset over aterm ctl\tactions=\tkey=harness.autopilot.s-8a81eae7abf4dbbd0e21\torigin=host\tend=\n\
m1\tkind=retired\tid=5\thow=folded\trec=1\tt=1793129339531\ttitle=Autopilot set to cruise\tdetail=Tab 1: the harness keeps going for ever, finding more to do\\uthis session's own setting\\uset over aterm ctl\trep=1\tend=\n\
m1\tkind=posted\tid=6\tt=1793129339636\ttag=harness\tsev=info\tglyph=ℹ\ttitle=Claude sign-in found no route\tdetail=aterm tried every sign-in route this machine has; none can make the login here, and each one tried is named\\uRun aterm harness accounts to read each route tried\\ulab-c: profile(none) mailbox(none configured) vault(not a member) headless-browser(none installed) hosts(none configured)\tactions=\tkey=harness.accounts.no-sign-in\torigin=host\tneed=later\tend=\n\
m1\tkind=retired\tid=1\thow=resolved:ok\tt=1793129939640\ttitle=Claude Code is not installed\tdetail=Claude Code is wanted on this machine, and installing it is your choice\\uInstall Claude Code in Settings\\unot installed\\uIn a terminal, aterm ai doctor signs in every AI CLI that needs it\trep=1\tend=\n\
m1\tkind=retired\tid=6\thow=resolved:ok\tt=1793130060990\ttitle=Claude sign-in found no route\tdetail=aterm tried every sign-in route this machine has; none can make the login here, and each one tried is named\\uRun aterm harness accounts to read each route tried\\ulab-c: profile(none) mailbox(none configured) vault(not a member) headless-browser(none installed) hosts(none configured)\trep=12\tend=\n";

#[test]
fn messages_log_reads_as_its_posts_retirements_and_folds() {
    let notes = notes_of(MESSAGES_RP6);
    assert_eq!(notes.len(), 9, "{notes:#?}");
    let no_route = &notes[6];
    assert_eq!(
        (
            no_route.kind.as_str(),
            no_route.id.as_str(),
            no_route.t_ms,
            no_route.key.as_str(),
            no_route.later
        ),
        (
            "posted",
            "6",
            1_793_129_339_636,
            "harness.accounts.no-sign-in",
            true
        )
    );
    assert_eq!(
        no_route
            .detail
            .last()
            .map(|d| d.starts_with("lab-c: profile(none)")),
        Some(true),
        "{:?}",
        no_route.detail
    );
    let folded = &notes[8];
    assert_eq!(
        (
            folded.kind.as_str(),
            folded.how.as_str(),
            folded.rep,
            folded.record
        ),
        ("retired", "resolved:ok", 12, false)
    );
    assert!(notes[5].record, "the autopilot record");
    assert!(!notes[4].later && notes[4].key.starts_with("harness.autopilot."));
    assert_eq!(
        notes_of("m1\tkind=dropped\tn=3\tend=\nnot a line\nm2\tkind=posted\n"),
        vec![Note {
            kind: "dropped".into(),
            dropped: 3,
            ..Note::default()
        }]
    );
    assert_eq!(unescape_field("a\\\\b\\tc\\ud\\qe"), "a\\b\tc\u{1f}d\\qe");
}

#[test]
fn nobody_asked_reads_the_whole_log_and_fails_on_an_ask_an_error_or_a_login() {
    let journal = journal_of(JOURNAL_R1);
    // rp6's own log: the no-route error before the first prompt is this run's too, allowed as
    // the recorded error after every route was tried and named with its folds.
    let held = judge_asked(&journal, &notes_of(MESSAGES_RP6));
    assert_eq!(held.verdict, Verdict::Ok, "{}", held.evidence);
    assert!(
        held.evidence
            .contains("harness.accounts.no-sign-in for lab-c ×12 (resolved:ok)"),
        "{}",
        held.evidence
    );
    assert!(held.evidence.contains("1 record(s)"), "{}", held.evidence);
    assert!(
        held.evidence.contains("ai.codex.not-installed ×1 (open)"),
        "{}",
        held.evidence
    );
    let fails = |text: &str, says: &str| {
        let j = judge_asked(&journal, &notes_of(text));
        assert_eq!(j.verdict, Verdict::Fail, "{says}: {}", j.evidence);
        assert!(j.evidence.contains(says), "{says}: {}", j.evidence);
    };
    // A message that is no record and no fact for later stood on the owner's screen, whenever
    // it was posted: the autopilot record, had it not been retired as one.
    let shown: String = MESSAGES_RP6
        .lines()
        .filter(|l| !l.contains("\trec=1\t"))
        .map(|l| format!("{l}\n"))
        .collect();
    fails(&shown, "on the owner's screen: `Autopilot set to cruise`");
    // An accounts fact for later other than the no-route error: the tap gone silent.
    fails(
        &MESSAGES_RP6.replace("harness.accounts.no-sign-in", "harness.accounts.no-tap"),
        "an accounts fact for later other than the recorded no-route error",
    );
    // An error, or a message naming /login, whatever its class.
    fails(
        &MESSAGES_RP6.replacen(
            "sev=info\tglyph=ℹ\ttitle=Claude sign-in",
            "sev=error\tglyph=ℹ\ttitle=Claude sign-in",
            1,
        ),
        "an error: `Claude sign-in found no route`",
    );
    fails(
        &MESSAGES_RP6.replace(
            "Run aterm harness accounts to read each route tried",
            "Type /login in the tab and sign in again.",
        ),
        "a message names /login: `Claude sign-in found no route`",
    );
    // A log that dropped lines hides what it lost.
    let lost = judge_asked(
        &journal,
        &notes_of(&format!("{MESSAGES_RP6}m1\tkind=dropped\tn=2\tend=\n")),
    );
    assert_eq!(lost.verdict, Verdict::Unproven, "{}", lost.evidence);
    for line in [
        "EXCEPTION seq=3 kind=login lab-b signed out",
        "ESCALATED seq=3 attention=login",
        "NEEDS seq=3 NEEDS YOU: sign in",
        "LEFT seq=3 rule=left-to-person@v1 the limit",
        "TYPED seq=3 rule=auth-login@v1 /login",
    ] {
        let mut j2 = journal.clone();
        j2.push(Said {
            t_ms: 1,
            line: line.into(),
        });
        let j = judge_asked(&j2, &[]);
        assert_eq!(j.verdict, Verdict::Fail, "{line}");
    }
}

/// Lab `rp6`'s harness acts on the tab's tape: R1's and R3's resume, each a fenced send into the
/// empty composer and one Enter on the row that holds it.
const ACTS_RP6: &str = r"1793129376397 send if-human=0 if-gen=3.396 if=^❯\s*$ -- [aterm harness] The usage limit has cleared; continue where you stopped.
1793129376912 key if-human=0 if-gen=3.408 if=^❯\x{A0}\[aterm\x20harness\]\x20The\x20usage\x20limit\x20has\x20cleared;\x20continue\x20where\x20you\x20stopped\.\s*$ enter
1793129706255 send if-human=0 if-gen=5.443 if=^❯\s*$ -- [aterm harness] The usage limit has cleared; continue where you stopped.
1793129706767 key if-human=0 if-gen=5.455 if=^❯\x{A0}\[aterm\x20harness\]\x20The\x20usage\x20limit\x20has\x20cleared;\x20continue\x20where\x20you\x20stopped\.\s*$ enter
";

#[test]
fn the_keys_hold_only_the_moves_resumes_and_cruises_rules_and_no_picker_key() {
    let journal = journal_of(JOURNAL_R1);
    let acts = acts_of(ACTS_RP6);
    let held = judge_keys(&journal, &acts);
    assert_eq!(held.verdict, Verdict::Ok, "{}", held.evidence);
    assert!(
        held.evidence.contains("usage-resume@v1"),
        "{}",
        held.evidence
    );
    assert!(
        held.evidence.contains("4 input act(s): 2 fenced send(s)"),
        "{}",
        held.evidence
    );
    let mut probe = journal.clone();
    probe.push(Said {
        t_ms: 1,
        line: "TYPED seq=9 rule=usage-probe@v1 /usage".into(),
    });
    let j = judge_keys(&probe, &acts);
    assert_eq!(j.verdict, Verdict::Fail);
    assert!(
        j.evidence.contains("typed for usage-probe@v1"),
        "{}",
        j.evidence
    );
    for act in [
        "1 send if=^❯\\s*$ -- /login",
        "1 key if=^❯\u{a0}/login\\s*$ enter",
        "1 key if-human=0 if-gen=3.408 if=^❯\\x{A0}/login\\s*$ enter",
        "1 send if=^❯\\s*$ -- /usage",
    ] {
        let j = judge_keys(&journal, &acts_of(act));
        assert_eq!(j.verdict, Verdict::Fail, "{act}: {}", j.evidence);
        assert!(
            j.evidence.contains("opens or answers a picker"),
            "{act}: {}",
            j.evidence
        );
    }
    // Every other input: an Enter on a question box's option row (the limit's options, one of
    // which may buy usage), a digit, an arrow, an Esc, a second Enter, an Enter with no send
    // before it, a send unfenced on the person or into a composer holding a draft, a paste.
    let send = ACTS_RP6.lines().next().unwrap_or_default();
    let enter = ACTS_RP6.lines().nth(1).unwrap_or_default();
    for extra in [
        r"1793129376913 key if-human=0 if-gen=3.409 if=^\x20{2}❯\x201\.\x20Stop\x20and\x20wait\x20for\x20limit\x20to\x20reset\s*$ enter",
        r"1793129376913 key if-human=0 if-gen=3.409 if=^❯\x201\.\x20Switch\x20to\x20extra\x20usage\s*$ enter",
        "1793129376913 key if-human=0 if-gen=3.409 if=^❯\\s*$ 1",
        "1793129376913 key if-human=0 if-gen=3.409 down",
        "1793129376913 key if-human=0 esc",
        enter,
        "1793129376913 send if-gen=3.409 if=^❯\\s*$ -- [aterm harness] keep going.",
        "1793129376913 send if-human=0 if-gen=3.409 if=^❯\\x{A0}draft\\s*$ -- keep going.",
        "1793129376913 paste if-human=0 -- keep going.",
    ] {
        let tape = format!("{send}\n{enter}\n{extra}\n");
        let j = judge_keys(&journal, &acts_of(&tape));
        assert_eq!(j.verdict, Verdict::Fail, "{extra}: {}", j.evidence);
        assert!(
            j.evidence
                .contains("an input act outside the composer's send and its one Enter"),
            "{extra}: {}",
            j.evidence
        );
    }
    // The send's Enter on an empty composer (its text gone) is no Enter on the row holding it.
    for empty in [
        r"1793129376912 key if-human=0 if-gen=3.408 if=^❯\s*$ enter",
        r"1793129376912 key if-human=0 if-gen=3.408 if=^❯\x{A0}\s*$ enter",
    ] {
        let j = judge_keys(&journal, &acts_of(&format!("{send}\n{empty}\n")));
        assert_eq!(j.verdict, Verdict::Fail, "{empty}: {}", j.evidence);
    }
    let alone = acts_of(&format!("{enter}\n"));
    assert_eq!(
        judge_keys(&journal, &alone).verdict,
        Verdict::Fail,
        "an Enter with no send"
    );
    // A wrapped composer's last row, and a guard a long row cut short, are still its own.
    let long = format!("{} and then some", "keep going ".repeat(4).trim_end());
    let wrapped = format!(
        "1 send if-human=0 if={} -- {long}\n2 key if-human=0 if=^\\x20{{2}}and\\x20then\\x20some\\s*$ enter\n",
        aterm_agent::supervise::policy::row_guard(COMPOSER)
    );
    assert_eq!(
        judge_keys(&journal, &acts_of(&wrapped)).verdict,
        Verdict::Ok,
        "{wrapped}"
    );
}

#[test]
fn the_fleet_names_three_rows_of_one_machine_and_the_synced_roster_is_checked_against_it() {
    let text = fleet_text();
    let fleet = FleetRoster::from_toml(&text, "lab").expect("the lab's fleet parses");
    let rows = fleet.accounts_for_machine(MACHINE);
    assert_eq!(
        rows.iter()
            .map(|a| (a.label.as_str(), a.email.as_str()))
            .collect::<Vec<_>>(),
        [(LABEL_A, EMAIL_A), (LABEL_B, EMAIL_B), (LABEL_C, EMAIL_C)]
    );
    assert!(text.contains("\nopt_in = \"named\"\n"), "{text}");
    assert_eq!(
        aterm_agent::harness::accounts_sync::normalize_machine_id(MACHINE).as_deref(),
        Some(MACHINE),
        "the machine is one a sync recognizes"
    );
    let (b, c) = (Path::new("/s/logins/lab-b"), Path::new("/s/logins/lab-c"));
    let synced = "[[account]]\nlabel = \"lab-a\"\nclass = \"dedicated\"\nemail = \"nobody-a@example.test\"\n\n\
                  [[account]]\nlabel = \"lab-b\"\nclass = \"dedicated\"\ndir = \"/s/logins/lab-b\"\nemail = \"nobody-b@example.test\"\n\n\
                  [[account]]\nlabel = \"lab-c\"\nclass = \"dedicated\"\ndir = \"/s/logins/lab-c\"\nemail = \"nobody-c@example.test\"\n";
    assert_eq!(roster_lacks(synced, (b, c)), None);
    // lab-a on a directory of its own is no default login's row.
    let own = synced.replacen(
        "email = \"nobody-a@example.test\"",
        "dir = \"/s/logins/lab-a\"\nemail = \"nobody-a@example.test\"",
        1,
    );
    assert!(
        roster_lacks(&own, (b, c)).is_some_and(|w| w.contains("lab-a is not the default")),
        "{:?}",
        roster_lacks(&own, (b, c))
    );
    // lab-c on another directory than the store's.
    let elsewhere = synced.replace("/s/logins/lab-c", "/elsewhere/lab-c");
    assert!(
        roster_lacks(&elsewhere, (b, c))
            .is_some_and(|w| w.contains("lab-c names /elsewhere/lab-c")),
        "{:?}",
        roster_lacks(&elsewhere, (b, c))
    );
    assert!(roster_lacks("[[account", (b, c)).is_some());
}

// ---------------------------------------------------------------------------------------------
// Lab `rp2` (Claude Code 2.1.311, 2026-10-27): R4 and the keychain tripwire.

/// rp2's local midnight: 2026-10-27 00:00 PDT.
const DAY2: i64 = 1_793_084_400_000;

/// lab-b's login directory in rp2.
const DIR_B2: &str = "/private/tmp/at.lab-501/aterm-8dd99024/runs/rp2/scratch/state/harness/claude-harness/accounts/logins/lab-b";

/// rp2's stub log from R3's served turn through R4's (their `RL200[…]` tails cut).
const STUB_LOG_R4: &str = r#"10:04:50.195 POST /v1/messages -> 200 body=55060B mode=ok kind=main model=claude-opus-5-5 stream=true max_tokens=128000 msgs=8 tools=13 sys=6082c last_user="Write one sentence about tides, then stop. [aterm harness] The usage ~" auth=oat:lab-a RL limit lifted (reset passed) sse=4 deltas
10:06:10.781 POST /v1/messages -> 429 body=53898B mode=limit:five_hour:1793120950 kind=main model=claude-opus-5-5 stream=true max_tokens=128000 msgs=11 tools=13 sys=6082c last_user="Write one sentence about gulls, then stop." auth=oat:lab-a RL limit:five_hour resets in 180s
10:10:10.842 POST /v1/messages -> 200 body=54117B mode=ok kind=main model=claude-opus-5-5 stream=true max_tokens=128000 msgs=11 tools=13 sys=6100c last_user="Write one sentence about gulls, then stop. Your claude.ai usage limit~" auth=oat:lab-a RL limit lifted (reset passed) sse=4 deltas
"#;

/// rp2's journal through R4: the wall, the wait on Claude Code's own continuation, and its turn.
const JOURNAL_R4: &str = r#"{"t":1793120771742,"sid":"s-054c7ddde0015744f6fe","kind":"event","phase":"limited","seq":958,"line":"EVENT limited seq=958 message=⚠ Usage limit reached · limit resets 10:09am — Continuing automatically at 10:09am · esc to cancel reset=10:09am"}
{"t":1793120771742,"sid":"s-054c7ddde0015744f6fe","kind":"limited","phase":"-","seq":958,"line":"LIMITED seq=958 handled: ⚠ Usage limit reached · limit resets 10:09am — Continuing automatically at 10:09am · esc to cancel"}
{"t":1793120771754,"sid":"s-054c7ddde0015744f6fe","kind":"waiting","phase":"-","seq":958,"line":"WAITING seq=958 until=2026-10-27T17:19:00Z usage-session goes on by itself"}
{"t":1793120950522,"sid":"s-054c7ddde0015744f6fe","kind":"event","phase":"limited","seq":964,"line":"EVENT limited seq=964 message=⚠ Usage limit reached · limit resets 10:09am — Continuing shortly · esc to cancel reset=10:09am"}
{"t":1793120950524,"sid":"s-054c7ddde0015744f6fe","kind":"waiting","phase":"-","seq":964,"line":"WAITING seq=964 until=2026-10-27T17:19:00Z usage-session goes on by itself"}
{"t":1793121011747,"sid":"s-054c7ddde0015744f6fe","kind":"cleared","phase":"-","seq":1439,"line":"CLEARED seq=1439 attention=none answered since the limit"}
{"t":1793121011747,"sid":"s-054c7ddde0015744f6fe","kind":"event","phase":"idle","seq":1439,"line":"EVENT idle seq=1439 ⏺ Hi! I am the local lab stub, not a model. (reply 7)"}
"#;

/// rp2's accounts' ledger at R4's wall.
const LEDGER_R4: &str = r#"{"account":"lab-a","kind":"struck","sid":"s-054c7ddde0015744f6fe","t":1793120771,"to":null,"why":"wall: five_hour"}
{"account":"lab-a","kind":"wall","sid":"s-054c7ddde0015744f6fe","t":1793120771,"to":null,"why":"five_hour until 1793120950"}
{"account":"lab-b","kind":"sign-in","sid":null,"t":1793120771,"to":null,"why":"auto no-route: profile(none) mailbox(none configured) vault(not a member) headless-browser(none installed) hosts(none configured)"}
"#;

/// rp2's aterm.log at R4.
const ATERM_LOG_R4: &str = "\
1793120771.737 15691 INFO aterm_gui::harness_host: harness: accounts-upkeep: account lab-b: automatic sign-in no-route: profile(none) mailbox(none configured) vault(not a member) headless-browser(none installed) hosts(none configured) (tried again in 15m or when one of them changes)
1793120771.739 15691 INFO aterm_gui::harness_host: harness @s-054c7ddde0015744f6fe: account advice: wait on lab-a (until 1793121100)
1793120950.518 15691 INFO aterm_gui::harness_host: harness @s-054c7ddde0015744f6fe: account advice: wait on lab-a (until 1793121100)
1793121011.743 15691 INFO aterm_gui::harness_host: harness @s-054c7ddde0015744f6fe: account lab-a served
";

/// The processes the watcher saw in rp2: the first launch, the move onto lab-b (R1) and the move
/// back onto lab-a (R3).
fn launches_rp2() -> Vec<Launched> {
    launches_of(&format!(
        "1793120324360 24735 - fresh\n1793120330974 26220 {DIR_B2} resume\n1793120657990 53637 - resume\n"
    ))
}

/// R4's evidence in rp2, with the resumed turn's history as the stub's dump holds it.
fn r4() -> (Ev, Leg) {
    let ev = Ev {
        journal: journal_of(JOURNAL_R4),
        ledger: rows_of(LEDGER_R4),
        aterm_log: ATERM_LOG_R4.into(),
        served: served_of(STUB_LOG_R4, DAY2),
        launches: launches_rp2(),
        drive_ledger: String::new(),
        dirs: vec![
            (LABEL_A.into(), DEFAULT_LOGIN.into()),
            (LABEL_B.into(), DIR_B2.into()),
        ],
        sid: "s-054c7ddde0015744f6fe".into(),
    };
    let leg = Leg {
        from: 1_793_120_770_375,
        to: 1_793_121_190_035,
        reset: 1_793_120_950,
        history: vec![
            P1.into(),
            format!(
                "{P2}\n\n[aterm harness] The usage limit has cleared; continue where you stopped."
            ),
            format!(
                "{P3}\n\n[aterm harness] The usage limit has cleared; continue where you stopped."
            ),
            format!("{P4}\n\nYour claude.ai usage limit"),
        ],
        ..Leg::default()
    };
    (ev, leg)
}

#[test]
fn r4_passes_on_the_runs_own_wait_and_sign_in_and_fails_on_a_move_or_none() {
    let (ev, leg) = r4();
    let held = judge_r4(&ev, &leg);
    assert_eq!(held.verdict, Verdict::Ok, "{}", held.evidence);
    assert!(
        held.evidence
            .contains("Claude Code's own continuation served on lab-a"),
        "{}",
        held.evidence
    );
    let fails = |ev: &Ev, leg: &Leg, says: &str| {
        let j = judge_r4(ev, leg);
        assert_eq!(j.verdict, Verdict::Fail, "{says}: {}", j.evidence);
        assert!(j.evidence.contains(says), "{says}: {}", j.evidence);
    };
    // The host's own resume, typed once Claude Code's continuation is late, holds too.
    let late = leg.reset * 1000 + 600_000;
    let mut typed = ev.clone();
    typed.journal.push(Said {
        t_ms: late,
        line: "CONTINUED seq=1500 rule=usage-resume@v1 [aterm harness] The usage limit has cleared; continue where you stopped.".into(),
    });
    for s in &mut typed.served {
        if s.status == 200 && s.t_ms > leg.reset * 1000 {
            s.t_ms = late + 1_000;
        }
    }
    let held = judge_r4(&typed, &leg);
    assert_eq!(held.verdict, Verdict::Ok, "{}", held.evidence);
    assert!(
        held.evidence.contains("the host's resume"),
        "{}",
        held.evidence
    );
    // A move: R4 has no row to move to.
    let mut moved = ev.clone();
    moved.journal.push(Said {
        t_ms: 1_793_120_800_000,
        line: "RESTARTED seq=960 rule=account-rotate@v1 relaunch under account lab-b (from the limit its turn ended on; the same conversation)".into(),
    });
    fails(&moved, &leg, "moved:");
    // No automatic sign-in of lab-b on record.
    let mut untried = ev.clone();
    untried.ledger.retain(|r| r.kind != "sign-in");
    fails(&untried, &leg, "no automatic `sign-in` row for lab-b");
    // A resume typed into the standing limit.
    let mut early = ev.clone();
    early.journal.push(Said {
        t_ms: leg.reset * 1000 - 60_000,
        line: "CONTINUED seq=961 rule=usage-resume@v1 [aterm harness] The usage limit has cleared; continue where you stopped.".into(),
    });
    fails(&early, &leg, "a resume typed before the reset");
    // The turn after the reset served under another row's credential.
    let mut elsewhere = ev.clone();
    for s in &mut elsewhere.served {
        if s.status == 200 && s.t_ms > leg.reset * 1000 {
            s.auth = AUTH_B.into();
        }
    }
    fails(&elsewhere, &leg, "carried oat:lab-b");
    // No turn served from the reset on.
    let mut none = ev.clone();
    none.served.retain(|s| s.t_ms < leg.reset * 1000);
    fails(&none, &leg, "no turn served after it under oat:lab-a");
    // A wait on another instant: the week's reset, far past the tab's own (its WAITING bounded
    // from above too), or the same instant for another reason than the usage-session wall.
    let mut weekly = ev.clone();
    for s in &mut weekly.journal {
        s.line = s
            .line
            .replace("until=2026-10-27T17:19:00Z", "until=2026-10-31T17:00:00Z");
    }
    fails(&weekly, &leg, "no WAITING record of the usage-session wall");
    let mut backoff = ev.clone();
    for s in &mut backoff.journal {
        s.line = s.line.replace(
            "usage-session goes on by itself",
            "2 short turn(s) in a row: 4 min back-off",
        );
    }
    fails(
        &backoff,
        &leg,
        "no WAITING record of the usage-session wall",
    );
    // The host's deadline at its latest still holds: the reset, Claude Code's own continuation's
    // grace and the minute its reset is named to.
    let mut latest = ev.clone();
    let limit = leg.reset + 600 + 60;
    let iso = |t: i64| format!("{}Z", crate::util::utc_civil(t));
    for s in &mut latest.journal {
        s.line = s.line.replace(
            "until=2026-10-27T17:19:00Z",
            &format!("until={}", iso(limit)),
        );
    }
    assert_eq!(judge_r4(&latest, &leg).verdict, Verdict::Ok);
    for s in &mut latest.journal {
        s.line = s.line.replace(
            &format!("until={}", iso(limit)),
            &format!("until={}", iso(limit + 1)),
        );
    }
    fails(&latest, &leg, "no WAITING record of the usage-session wall");
    // No WAITING record naming the reset: a wait nobody can see.
    let mut unsaid = ev;
    unsaid.journal.retain(|s| !s.line.starts_with("WAITING "));
    fails(&unsaid, &leg, "no WAITING record");
}

/// rp2's keychain tripwire: each ask with the pid that asked and its parent, the lab's launch.
const SECURITY_LOG_RP2: &str = r#"09:58:44 find-generic-password account="owner" service="Claude Code-credentials" via=argv pid=24783 ppid=24735
09:58:44 find-generic-password account="owner" service="Claude Code" via=argv pid=24785 ppid=24735
09:58:44 find-generic-password account="owner" service="Claude Code-credentials" via=argv pid=24805 ppid=24735
09:58:50 find-generic-password account="owner" service="Claude Code-credentials-f7cada39" via=argv pid=26232 ppid=26220
09:58:50 find-generic-password account="owner" service="Claude Code-f7cada39" via=argv pid=26233 ppid=26220
09:58:51 find-generic-password account="owner" service="Claude Code-credentials-f7cada39" via=argv pid=26258 ppid=26220
09:58:51 find-generic-password account="owner" service="Claude Code-credentials-f7cada39" via=argv pid=26259 ppid=26220
10:04:17 find-generic-password account="owner" service="Claude Code-credentials" via=argv pid=53639 ppid=53637
10:04:17 find-generic-password account="owner" service="Claude Code" via=argv pid=53640 ppid=53637
10:04:18 find-generic-password account="owner" service="Claude Code-credentials" via=argv pid=53644 ppid=53637
10:04:18 find-generic-password account="owner" service="Claude Code-credentials" via=argv pid=53645 ppid=53637
"#;

/// rp2's jail, its keychain rules.
const JAIL_RP2: &str = r#"(deny mach-lookup (global-name "com.apple.SecurityServer"))
(deny process-exec (literal "/usr/bin/security"))
(deny file-read* file-write* (subpath "/Users//owner/Library/Keychains"))
"#;

#[test]
fn the_keychain_judge_reads_each_ask_by_the_launch_that_made_it() {
    let launches = launches_rp2();
    let held = judge_keychain(SECURITY_LOG_RP2, &launches, JAIL_RP2);
    assert_eq!(held.verdict, Verdict::Ok, "{}", held.evidence);
    assert!(
        held.evidence
            .starts_with("7 un-suffixed ask(s) by the default login's launches, 4 suffixed"),
        "{}",
        held.evidence
    );
    let fails = |log: &str, launches: &[Launched], jail: &str, says: &str| {
        let j = judge_keychain(log, launches, jail);
        assert_eq!(j.verdict, Verdict::Fail, "{says}: {}", j.evidence);
        assert!(j.evidence.contains(says), "{says}: {}", j.evidence);
    };
    // A launch on lab-b's login asks for the owner's un-suffixed item.
    let owners = SECURITY_LOG_RP2.replace(
        "service=\"Claude Code-f7cada39\" via=argv pid=26233",
        "service=\"Claude Code\" via=argv pid=26233",
    );
    fails(
        &owners,
        &launches,
        JAIL_RP2,
        "asked for the owner's `Claude Code`",
    );
    // The default login asks for a login directory's item.
    let suffixed = SECURITY_LOG_RP2.replace(
        "service=\"Claude Code\" via=argv pid=24785",
        "service=\"Claude Code-f7cada39\" via=argv pid=24785",
    );
    fails(
        &suffixed,
        &launches,
        JAIL_RP2,
        "the default login asked for",
    );
    // An owner's item asked for by a process no lab launch is the parent of.
    let stray = SECURITY_LOG_RP2.replace("pid=24783 ppid=24735", "pid=24783 ppid=1");
    fails(&stray, &launches, JAIL_RP2, "no lab launch's child");
    // A jail that leaves the keychain's daemon reachable.
    let open = JAIL_RP2.replace(
        "(deny mach-lookup (global-name \"com.apple.SecurityServer\"))\n",
        "",
    );
    fails(SECURITY_LOG_RP2, &launches, &open, "the jail lacks");
    // Only the default login's launches asked: the login directories' side is unproven.
    let one_side: String = SECURITY_LOG_RP2
        .lines()
        .filter(|l| !l.contains("ppid=26220"))
        .map(|l| format!("{l}\n"))
        .collect();
    assert_eq!(
        judge_keychain(&one_side, &launches, JAIL_RP2).verdict,
        Verdict::Unproven
    );
}

// ---------------------------------------------------------------------------------------------
// Lab `rp3` (Claude Code 2.1.311, 2026-10-27): R5a, R5b and R2 (its local midnight is DAY2's).

/// `aterm harness accounts --json` just before R5a's sign-in as lab-b, each row cut to the fields
/// [`listed`] reads (and `active`): lab-b's login gone since R4, its automatic sign-in failed.
const LISTING_A_DEFAULT: &str = r#"{"accounts":[{"active":true,"auth":"ok","condition":"limited","label":"lab-a","login":"ok","sign_in_attempt":null},{"active":false,"auth":"ok","condition":"not-enrolled","label":"lab-b","login":"ok","sign_in_attempt":{"outcome":"no-route","since":1793124278}},{"active":false,"auth":"unseen","condition":"not-enrolled","label":"lab-c","login":"idle","sign_in_attempt":{"outcome":"no-route","since":1793123908}}]}"#;

/// The listing after R5a's turn, which is also the one before R5b's sign-in as lab-a: lab-a
/// displaced, so signed out with its login expired and its automatic sign-in failed (the owner's
/// `m2-2`).
const LISTING_B_DEFAULT: &str = r#"{"accounts":[{"active":false,"auth":"ok","condition":"usable","label":"lab-b","login":"ok","sign_in_attempt":null},{"active":true,"auth":"signed-out","condition":"not-enrolled","label":"lab-a","login":"expired","sign_in_attempt":{"outcome":"no-route","since":1793124568}},{"active":false,"auth":"unseen","condition":"not-enrolled","label":"lab-c","login":"idle","sign_in_attempt":{"outcome":"no-route","since":1793123908}}]}"#;

/// The listing after R5b's turn: lab-a usable on the default login again, lab-b displaced.
const LISTING_A_AGAIN: &str = r#"{"accounts":[{"active":true,"auth":"ok","condition":"usable","label":"lab-a","login":"ok","sign_in_attempt":null},{"active":false,"auth":"signed-out","condition":"not-enrolled","label":"lab-b","login":"expired","sign_in_attempt":null},{"active":false,"auth":"unseen","condition":"not-enrolled","label":"lab-c","login":"idle","sign_in_attempt":{"outcome":"no-route","since":1793123908}}]}"#;

/// rp3's stub log over R5a's, R5b's and R2's P7 turns (their tails cut).
const STUB_LOG_R5: &str = r#"11:09:28.947 POST /v1/messages -> 200 body=54362B mode=ok kind=main model=claude-opus-5-5 stream=true max_tokens=128000 msgs=14 tools=13 sys=6082c last_user="Write one sentence about anchors, then stop." auth=oat:lab-b sse=4 deltas
11:10:28.880 POST /v1/messages -> 200 body=54623B mode=ok kind=main model=claude-opus-5-5 stream=true max_tokens=128000 msgs=17 tools=13 sys=6082c last_user="Write one sentence about sails, then stop." auth=oat:lab-a limit lifted (reset passed) sse=4 deltas
11:10:29.828 POST /v1/messages -> 200 body=54884B mode=ok kind=main model=claude-opus-5-5 stream=true max_tokens=128000 msgs=20 tools=13 sys=6082c last_user="Write one sentence about buoys, then stop." auth=oat:lab-a limit lifted (reset passed) sse=4 deltas
"#;

/// rp3's journal from R5a to R2's move.
const JOURNAL_R5: &str = r#"{"t":1793124549044,"sid":"s-2764af3cd2a43e00bc0c","kind":"cleared","phase":"-","seq":1412,"line":"CLEARED seq=1412 attention=none answered since the limit"}
{"t":1793124549044,"sid":"s-2764af3cd2a43e00bc0c","kind":"event","phase":"idle","seq":1412,"line":"EVENT idle seq=1412 ⏺ Hi! I am the local lab stub, not a model. (reply 7)"}
{"t":1793124549568,"sid":"s-2764af3cd2a43e00bc0c","kind":"waiting","phase":"-","seq":1412,"line":"WAITING seq=1412 until=2026-10-27T18:13:09Z 2 short turn(s) in a row: 4 min back-off"}
{"t":1793124570611,"sid":"s-2764af3cd2a43e00bc0c","kind":"event","phase":"idle","seq":1758,"line":"EVENT idle seq=1758 ⏺ Hi! I am the local lab stub, not a model. (reply 8)"}
{"t":1793124631425,"sid":"s-2764af3cd2a43e00bc0c","kind":"event","phase":"idle","seq":2469,"line":"EVENT idle seq=2469 ⏺ Hi! I am the local lab stub, not a model. (reply 10)"}
{"t":1793124633345,"sid":"s-2764af3cd2a43e00bc0c","kind":"other","phase":"-","seq":null,"line":"HOST seq=2469 restart:account-ahead step=adopted"}
{"t":1793124633348,"sid":"s-2764af3cd2a43e00bc0c","kind":"restarted","phase":"account-rotate@v1","seq":2469,"line":"RESTARTED seq=2469 rule=account-rotate@v1 relaunch under account lab-c (at a turn end; the same conversation)"}
"#;

/// rp3's accounts' ledger from R4's wall to R2's move.
const LEDGER_R5: &str = r#"{"account":"lab-b","kind":"sign-in","sid":null,"t":1793124278,"to":null,"why":"auto no-route: profile(none) mailbox(none configured) vault(not a member) headless-browser(none installed) hosts(none configured)"}
{"account":"lab-a","kind":"wall","sid":"s-2764af3cd2a43e00bc0c","t":1793124278,"to":null,"why":"five_hour until 1793124457"}
{"account":"lab-a","kind":"displaced","sid":null,"t":1793124568,"to":"lab-b","why":"default login now lab-b"}
{"account":"lab-a","kind":"sign-in","sid":null,"t":1793124568,"to":null,"why":"auto no-route: profile(none) mailbox(none configured) vault(not a member) headless-browser(none installed) hosts(none configured)"}
{"account":"lab-a","kind":"auth","sid":null,"t":1793124628,"to":null,"why":"ok"}
{"account":"lab-b","kind":"displaced","sid":null,"t":1793124628,"to":"lab-a","why":"default login now lab-a"}
{"account":"lab-a","kind":"warned","sid":"s-2764af3cd2a43e00bc0c","t":1793124630,"to":null,"why":"session 95%"}
{"account":"lab-a","kind":"rotate","sid":"s-2764af3cd2a43e00bc0c","t":1793124631,"to":"lab-c","why":""}
{"account":"lab-a","kind":"move","sid":"s-2764af3cd2a43e00bc0c","t":1793124633,"to":"lab-c","why":""}
"#;

/// The user texts every rp3 turn from R4's on carried before the scenario's own prompt.
fn rp3_history() -> Vec<String> {
    vec![
        P1.into(),
        format!("{P2}\n\n[aterm harness] The usage limit has cleared; continue where you stopped."),
        format!("{P3}\n\n[aterm harness] The usage limit has cleared; continue where you stopped."),
        format!("{P4}\n\nYour claude.ai usage limit has reset."),
    ]
}

/// R5a's and R5b's evidence in rp3, with each leg's listings and its turn's history.
fn r5() -> (Ev, Leg, Leg) {
    let ev = Ev {
        journal: journal_of(JOURNAL_R5),
        ledger: rows_of(LEDGER_R5),
        served: served_of(STUB_LOG_R5, DAY2),
        ..Ev::default()
    };
    let mut history = rp3_history();
    history.push(P5.into());
    let r5a = Leg {
        from: 1_793_124_548_366,
        to: 1_793_124_569_468,
        asked: 1_793_124_568_530,
        history: history.clone(),
        before: LISTING_A_DEFAULT.into(),
        listing: LISTING_B_DEFAULT.into(),
        ..Leg::default()
    };
    history.push(P6.into());
    let r5b = Leg {
        from: 1_793_124_569_486,
        to: 1_793_124_629_426,
        asked: 1_793_124_628_444,
        history,
        before: LISTING_B_DEFAULT.into(),
        listing: LISTING_A_AGAIN.into(),
        ..Leg::default()
    };
    (ev, r5a, r5b)
}

const R5A: Displacement<'static> = Displacement {
    from: LABEL_A,
    to: LABEL_B,
    auth: AUTH_B,
    prompts: &[P1, P5],
};

const R5B: Displacement<'static> = Displacement {
    from: LABEL_B,
    to: LABEL_A,
    auth: AUTH_A,
    prompts: &[P1, P6],
};

#[test]
fn the_instances_listing_reads_as_each_rows_words() {
    assert_eq!(
        listed(LISTING_B_DEFAULT, LABEL_A),
        Some(Listed {
            auth: "signed-out".into(),
            login: "expired".into(),
            condition: "not-enrolled".into(),
            sign_in: "no-route".into(),
        })
    );
    assert_eq!(
        listed(LISTING_B_DEFAULT, LABEL_B),
        Some(Listed {
            auth: "ok".into(),
            login: "ok".into(),
            condition: "usable".into(),
            sign_in: String::new(),
        })
    );
    assert_eq!(
        listed(LISTING_A_DEFAULT, LABEL_C).map(|r| (r.auth, r.login)),
        Some(("unseen".into(), "idle".into()))
    );
    assert_eq!(listed(LISTING_B_DEFAULT, "lab-d"), None);
    assert_eq!(listed("ERR no such session", LABEL_A), None);
    // `--cli all`'s shape nests the rows under `pools`: no `accounts` array at the top.
    assert_eq!(
        listed(&format!("{{\"pools\":[{LISTING_B_DEFAULT}]}}"), LABEL_A),
        None
    );
}

#[test]
fn r5_passes_on_the_runs_own_displacements_and_fails_on_a_stale_row_or_a_turn_elsewhere() {
    let (ev, r5a, r5b) = r5();
    let held = judge_r5(&ev, &r5a, R5A);
    assert_eq!(held.verdict, Verdict::Ok, "{}", held.evidence);
    assert!(
        held.evidence.contains(
            "lab-b was auth ok login ok not-enrolled sign-in no-route, now auth ok login ok usable"
        ),
        "{}",
        held.evidence
    );
    let held = judge_r5(&ev, &r5b, R5B);
    assert_eq!(held.verdict, Verdict::Ok, "{}", held.evidence);
    assert!(
        held.evidence
            .contains("lab-a was auth signed-out login expired"),
        "{}",
        held.evidence
    );
    let fails = |ev: &Ev, leg: &Leg, d: Displacement<'_>, says: &str| {
        let j = judge_r5(ev, leg, d);
        assert_eq!(j.verdict, Verdict::Fail, "{says}: {}", j.evidence);
        assert!(j.evidence.contains(says), "{says}: {}", j.evidence);
    };
    // The owner's second incident as a sync with no fix leaves it: the row the default login
    // names again still listed signed out, expired, with its failed sign-in.
    let stale = Leg {
        listing: LISTING_B_DEFAULT.into(),
        ..r5b.clone()
    };
    fails(&ev, &stale, R5B, "lab-a listed auth signed-out");
    fails(&ev, &stale, R5B, "lab-a listed login expired");
    fails(&ev, &stale, R5B, "lab-a listed a failed sign-in (no-route)");
    // The turn after the sign-in served under the displaced row's credential.
    let mut elsewhere = ev.clone();
    for s in &mut elsewhere.served {
        if s.last_user.starts_with(P6) {
            s.auth = AUTH_B.into();
        }
    }
    fails(&elsewhere, &r5b, R5B, "carried oat:lab-b");
    // No displacement on record.
    let mut kept = ev.clone();
    kept.ledger.retain(|r| r.kind != "displaced");
    fails(&kept, &r5b, R5B, "no `displaced` row lab-b → lab-a");
    // A signed-out row signed in again without its `auth ok`.
    let mut unsaid = ev.clone();
    unsaid
        .ledger
        .retain(|r| !(r.account == LABEL_A && r.kind == "auth"));
    fails(&unsaid, &r5b, R5B, "no `auth ok` row for lab-a");
    // A sign-in tried of the row the default login already holds.
    let mut tried = ev.clone();
    tried.ledger.push(Row {
        t: 1_793_124_629,
        account: LABEL_A.into(),
        kind: "sign-in".into(),
        to: String::new(),
        why: "auto no-route: profile(none)".into(),
    });
    fails(
        &tried,
        &r5b,
        R5B,
        "a sign-in of lab-a tried after it was signed in",
    );
    // The session moved instead: the default login's sign-in is no rotation.
    let mut moved = ev.clone();
    moved.journal.push(Said {
        t_ms: 1_793_124_600_000,
        line: "RESTARTED seq=1800 rule=account-rotate@v1 relaunch under account lab-a (from the limit its turn ended on; the same conversation)".into(),
    });
    fails(&moved, &r5b, R5B, "restarted:");
    // The turn's history lost the conversation.
    let lost = Leg {
        history: vec![P6.into()],
        ..r5b.clone()
    };
    fails(
        &ev,
        &lost,
        R5B,
        "the history lacks `Write one sentence about lighthouses",
    );
    // Nothing stale stood against the row before: a sync with no fix passes that too.
    let fresh = Leg {
        before: LISTING_A_AGAIN.into(),
        ..r5b
    };
    let j = judge_r5(&ev, &fresh, R5B);
    assert_eq!(j.verdict, Verdict::Unproven, "{}", j.evidence);
    assert!(
        j.evidence.contains("nothing stale stood against lab-a"),
        "{}",
        j.evidence
    );
}

// ---------------------------------------------------------------------------------------------
// Lab `rp6` (Claude Code 2.1.311, 2026-10-27): R2 (its local midnight is DAY2's).

/// lab-c's login directory in rp6.
const DIR_C6: &str = "/private/tmp/at.lab-501/aterm-8dd99024/runs/rp6/scratch/state/harness/claude-harness/accounts/logins/lab-c";

/// rp6's journal at R2's move: P7's turn end, the move ahead, the new process's start, P8's.
const JOURNAL_R2: &str = r#"{"t":1793130061546,"sid":"s-8a81eae7abf4dbbd0e21","kind":"event","phase":"idle","seq":2495,"line":"EVENT idle seq=2495 ⏺ Hi! I am the local lab stub, not a model. (reply 10)"}
{"t":1793130061548,"sid":"s-8a81eae7abf4dbbd0e21","kind":"skipped","phase":"usage-probe@v1","seq":2495,"line":"SKIPPED seq=2495 rule=usage-probe@v1 gate=not-live"}
{"t":1793130063257,"sid":"s-8a81eae7abf4dbbd0e21","kind":"other","phase":"-","seq":null,"line":"HOST seq=2495 restart:account-ahead step=adopted"}
{"t":1793130063260,"sid":"s-8a81eae7abf4dbbd0e21","kind":"restarted","phase":"account-rotate@v1","seq":2495,"line":"RESTARTED seq=2495 rule=account-rotate@v1 relaunch under account lab-c (at a turn end; the same conversation)"}
{"t":1793130063273,"sid":"s-8a81eae7abf4dbbd0e21","kind":"other","phase":"-","seq":null,"line":"HOST seq=2495 carry-on step=done:resumed"}
{"t":1793130063787,"sid":"s-8a81eae7abf4dbbd0e21","kind":"waiting","phase":"-","seq":457,"line":"WAITING seq=457 until=2026-10-27T19:43:09Z 2 short turn(s) in a row: 4 min back-off"}
{"t":1793130070150,"sid":"s-8a81eae7abf4dbbd0e21","kind":"event","phase":"idle","seq":722,"line":"EVENT idle seq=722 ⏺ Hi! I am the local lab stub, not a model. (reply 11)"}
"#;

/// rp6's accounts' ledger from R5b's displacement through R2: Claude Code's warning at 95 %, read
/// off its screen (no tap reading of it was booked), the rotation, the move and `lab-c` proven.
const LEDGER_R2: &str = r#"{"account":"lab-a","kind":"auth","sid":null,"t":1793130059,"to":null,"why":"ok"}
{"account":"lab-b","kind":"displaced","sid":null,"t":1793130059,"to":"lab-a","why":"default login now lab-a"}
{"account":"lab-a","kind":"warned","sid":"s-8a81eae7abf4dbbd0e21","t":1793130060,"to":null,"why":"session 95%"}
{"account":"lab-a","kind":"rotate","sid":"s-8a81eae7abf4dbbd0e21","t":1793130061,"to":"lab-c","why":""}
{"account":"lab-a","kind":"move","sid":"s-8a81eae7abf4dbbd0e21","t":1793130063,"to":"lab-c","why":""}
{"account":"lab-c","kind":"sample","sid":"s-8a81eae7abf4dbbd0e21","t":1793130070,"to":null,"why":"tap: session 10% week 20%"}
{"account":"lab-c","kind":"proven","sid":"s-8a81eae7abf4dbbd0e21","t":1793130070,"to":null,"why":""}
"#;

/// rp6's stub log: R5b's turn, P7's on lab-a at its high figure, P8's on lab-c (tails cut).
const STUB_LOG_R2: &str = r#"12:40:59.779 POST /v1/messages -> 200 body=54623B mode=ok kind=main model=claude-opus-5-5 stream=true max_tokens=128000 msgs=17 tools=13 sys=6082c last_user="Write one sentence about sails, then stop." auth=oat:lab-a limit lifted (reset passed) sse=4 deltas
12:41:00.707 POST /v1/messages -> 200 body=54884B mode=ok kind=main model=claude-opus-5-5 stream=true max_tokens=128000 msgs=20 tools=13 sys=6082c last_user="Write one sentence about buoys, then stop." auth=oat:lab-a limit lifted (reset passed) sse=4 deltas
12:41:04.778 POST /v1/messages -> 200 body=56571B mode=ok kind=main model=claude-opus-5-5 stream=true max_tokens=128000 msgs=23 tools=13 sys=6082c last_user="Write one sentence about jetties, then stop." auth=oat:lab-c sse=4 deltas
"#;

/// rp6's aterm.log at R2: the advice, the move ahead's adopted process, lab-c proven.
const ATERM_LOG_R2: &str = "\
1793130061.540 32570 INFO aterm_gui::harness_host: harness @s-8a81eae7abf4dbbd0e21: account advice: move to lab-c
1793130063.213 32570 INFO aterm_gui::harness_host: harness @s-8a81eae7abf4dbbd0e21: restart:account-ahead pid=17973 tab=s-8a81eae7abf4dbbd0e21 session=66729fd9-3b40-490d-9161-2cef6e1326cb from=2.1.311 to=2.1.311(same) step=adopted
1793130070.117 32570 INFO aterm_gui::harness_host: harness @s-8a81eae7abf4dbbd0e21: account lab-c proven
";

/// rp6's upgrade ledger at R2: the move ahead's step on the process it ended, and the process it
/// adopted (their details cut).
const DRIVE_LEDGER_R2: &str = r#"{"detail":"SIGTERM","from":"2.1.311","pid":89687,"session":"66729fd9-3b40-490d-9161-2cef6e1326cb","step":"restart:account-ahead","t":1793130061,"tab":"s-8a81eae7abf4dbbd0e21","to":"2.1.311(same)"}
{"detail":"","from":"2.1.311","pid":89687,"session":"66729fd9-3b40-490d-9161-2cef6e1326cb","step":"relaunched","t":1793130062,"tab":"s-8a81eae7abf4dbbd0e21","to":"2.1.311(same)"}
{"detail":"the relaunched agent holds the conversation; resumed while idle, nothing typed","from":"2.1.311","pid":17973,"session":"66729fd9-3b40-490d-9161-2cef6e1326cb","step":"adopted","t":1793130063,"tab":"s-8a81eae7abf4dbbd0e21","to":"2.1.311(same)"}
"#;

/// R2's evidence in rp6, with P8's turn's history as the stub's dump holds it.
fn r2() -> (Ev, Leg) {
    let ev = Ev {
        journal: journal_of(JOURNAL_R2),
        ledger: rows_of(LEDGER_R2),
        aterm_log: ATERM_LOG_R2.into(),
        served: served_of(STUB_LOG_R2, DAY2),
        launches: launches_of(&format!("1793130062937 17973 {DIR_C6} resume\n")),
        drive_ledger: DRIVE_LEDGER_R2.into(),
        dirs: vec![
            (LABEL_A.into(), DEFAULT_LOGIN.into()),
            (LABEL_C.into(), DIR_C6.into()),
        ],
        sid: "s-8a81eae7abf4dbbd0e21".into(),
    };
    let mut history = rp3_history();
    history.extend([P5.into(), P6.into(), P7.into(), P8.into()]);
    let leg = Leg {
        from: 1_793_130_060_332,
        to: 1_793_130_070_492,
        asked: 1_793_130_060_332,
        history,
        ..Leg::default()
    };
    (ev, leg)
}

/// rp8's accounts' ledger at R2, once the tap read a tab moved back onto the default login: the
/// tap's reading of lab-a at 95 %, booked in the second of the rotation it made.
const LEDGER_R2_RP8: &str = r#"{"account":"lab-a","kind":"sample","sid":"s-e4338e894386e6579314","t":1793140679,"to":null,"why":"tap: session 95% week 20%"}
{"account":"lab-a","kind":"rotate","sid":"s-e4338e894386e6579314","t":1793140679,"to":"lab-c","why":""}
"#;

/// A tap `sample` row of lab-a at `pct` % booked at `t`, in the shape rp8's ledger booked one
/// ([`LEDGER_R2_RP8`]).
fn tapped(t: i64, pct: u32) -> Row {
    Row {
        t,
        account: LABEL_A.into(),
        kind: "sample".into(),
        to: String::new(),
        why: format!("tap: session {pct}% week 20%"),
    }
}

#[test]
fn r2_passes_on_the_taps_move_ahead_and_fails_on_a_screen_warning_alone_a_wall_or_another_row() {
    let (rp6, leg) = r2();
    // rp6's own ledger: the move came on Claude Code's warning read off its screen; the tap's
    // reading of 95 % was refused (the tab was back on the default login with the login variable
    // set empty), so nothing proves the tap moves a tab ahead of its limit.
    let j = judge_r2(&rp6, &leg);
    assert_eq!(j.verdict, Verdict::Fail, "{}", j.evidence);
    assert!(
        j.evidence
            .contains("no tap `sample` row for lab-a at 90 % or more before the rotation"),
        "{}",
        j.evidence
    );
    // The row a live tap books reads as the one these controls plant.
    assert_eq!(
        rows_of(LEDGER_R2_RP8).first(),
        Some(&tapped(1_793_140_679, 95))
    );
    // The tap's reading booked by the rotation's second: the move ahead is the tap's.
    let mut ev = rp6.clone();
    ev.ledger.insert(2, tapped(1_793_130_060, 95));
    let held = judge_r2(&ev, &leg);
    assert_eq!(held.verdict, Verdict::Ok, "{}", held.evidence);
    assert!(
        held.evidence.contains(
            "lab-a read at 95 % by the tap (Claude Code's warning read off its screen too)"
        ),
        "{}",
        held.evidence
    );
    assert!(
        held.evidence
            .contains("relaunch under account lab-c (at a tur"),
        "{}",
        held.evidence
    );
    let fails = |ev: &Ev, leg: &Leg, says: &str| {
        let j = judge_r2(ev, leg);
        assert_eq!(j.verdict, Verdict::Fail, "{says}: {}", j.evidence);
        assert!(j.evidence.contains(says), "{says}: {}", j.evidence);
    };
    // The tap's reading booked only after the move, or below the low mark.
    let no_tap = "no tap `sample` row for lab-a at 90 % or more before the rotation";
    for row in [tapped(1_793_130_062, 95), tapped(1_793_130_060, 89)] {
        let mut late = rp6.clone();
        late.ledger.push(row);
        fails(&late, &leg, no_tap);
    }
    // Labs rp4 and rp5, before the move's answer was read off a prompt answered at once: lab-c
    // served P8, but the move was never told so and no `proven` row was booked.
    let mut unproven = ev.clone();
    unproven.ledger.retain(|r| r.kind != "proven");
    fails(
        &unproven,
        &leg,
        "no `proven` row for lab-c after the rotation",
    );
    // The upgrade ledger booked a wall move's step, or none, for this move ahead.
    let mut walls_step = ev.clone();
    walls_step.drive_ledger = DRIVE_LEDGER_R2.replace("restart:account-ahead", "restart:account");
    fails(
        &walls_step,
        &leg,
        "the upgrade ledger has no restart:account-ahead step of the tab in the window",
    );
    // lab-a walled in the window: no move ahead of a wall.
    let mut walled = ev.clone();
    walled.served[1].status = 429;
    fails(&walled, &leg, "lab-a was walled in the window");
    // A move at a limit, not at a turn end.
    let mut at_wall = ev.clone();
    for s in &mut at_wall.journal {
        s.line = s.line.replace(
            "(at a turn end; the same conversation)",
            "(from the limit its turn ended on; the same conversation)",
        );
    }
    fails(&at_wall, &leg, "the restart is not at a turn end");
    // The adopted process on another login than lab-c's.
    let mut elsewhere = ev.clone();
    elsewhere.launches = launches_of(&format!("1793130062937 17973 {DIR_B2} resume\n"));
    fails(&elsewhere, &leg, "runs on login");
    // No process adopted on record.
    let mut unadopted = ev.clone();
    unadopted.aterm_log = unadopted.aterm_log.replace("step=adopted", "step=refused");
    fails(
        &unadopted,
        &leg,
        "names no process a restart:account adopted",
    );
    // Lab rp3, before the lab typed P8: the move at P7's turn end left no turn to serve.
    let mut none = ev.clone();
    none.served.retain(|s| s.auth != AUTH_C);
    fails(&none, &leg, "no turn served after it under oat:lab-c");
    // The next turn on lab-a's credential.
    let mut stayed = ev.clone();
    for s in &mut stayed.served {
        if s.auth == AUTH_C {
            s.auth = AUTH_A.into();
        }
    }
    fails(&stayed, &leg, "carried oat:lab-a");
    // The turn's history lost P8.
    let lost = Leg {
        history: rp3_history(),
        ..leg
    };
    fails(
        &ev,
        &lost,
        "the history lacks `Write one sentence about jetties",
    );
}

// ---------------------------------------------------------------------------------------------
// Lab `rp7` (Claude Code 2.1.311, 2026-10-27, `--no-route-seam`): the live negative control (its
// local midnight is DAY2's).

/// rp7's stub log: P1's turn on lab-a, then lab-a's wall on P2 (their `RL200[…]` tails cut).
const STUB_LOG_RP7: &str = r#"12:42:22.309 POST /v1/messages -> 200 body=4105B mode=ok kind=aux model=claude-opus-5-5 stream=true max_tokens=128000 msgs=1 tools=0 sys=3188c last_user="<session> Write one sentence about lighthouses, then stop. </session>~" auth=oat:lab-a sse=4 deltas
12:42:22.309 POST /v1/messages -> 200 body=54179B mode=ok kind=main model=claude-opus-5-5 stream=true max_tokens=128000 msgs=2 tools=13 sys=6082c last_user="<system-reminder> As you answer the user's questions, you can use the~" auth=oat:lab-a sse=4 deltas
12:42:23.201 POST /v1/messages -> 429 body=4102B mode=limit:five_hour:1793130322 kind=aux model=claude-opus-5-5 stream=true max_tokens=128000 msgs=1 tools=0 sys=3188c last_user="<session> Write one sentence about harbours, then stop. </session> Wr~" auth=oat:lab-a limit:five_hour resets in 179s
12:42:23.201 POST /v1/messages -> 429 body=53020B mode=limit:five_hour:1793130322 kind=main model=claude-opus-5-5 stream=true max_tokens=128000 msgs=5 tools=13 sys=6082c last_user="Write one sentence about harbours, then stop." auth=oat:lab-a limit:five_hour resets in 179s
"#;

/// rp7's aterm.log at R1's wall: the host reads the tab as no stored login's.
const ATERM_LOG_RP7: &str = "\
1793130143.138 18984 INFO aterm_gui::harness_host: harness @s-032f8f6564075123606c: usage tap: reading not this machine's own login's
1793130144.046 18984 INFO aterm_gui::harness_host: harness @s-032f8f6564075123606c: account advice: stay (not-a-stored-login)
1793130144.052 18984 INFO aterm_gui::harness_host: harness @s-032f8f6564075123606c: account wall not-stored
";

/// rp7's journal at R1's wall: the limit and Claude Code's own continuation waited on, no move.
const JOURNAL_RP7: &str = r#"{"t":1793130144048,"sid":"s-032f8f6564075123606c","kind":"limited","phase":"-","seq":662,"line":"LIMITED seq=662 handled: ⚠ Usage limit reached · limit resets 12:45pm — Continuing automatically at 12:45pm · esc to cancel"}
{"t":1793130144052,"sid":"s-032f8f6564075123606c","kind":"waiting","phase":"-","seq":662,"line":"WAITING seq=662 until=2026-10-27T19:55:00Z usage-session goes on by itself"}
"#;

/// rp7's evidence, its R1 leg, and its first prompt's instant.
fn rp7() -> (Ev, Leg, i64) {
    let ev = Ev {
        journal: journal_of(JOURNAL_RP7),
        aterm_log: ATERM_LOG_RP7.into(),
        served: served_of(STUB_LOG_RP7, DAY2),
        sid: "s-032f8f6564075123606c".into(),
        ..Ev::default()
    };
    let r1 = Leg {
        from: 1_793_130_142_821,
        to: 1_793_130_272_280,
        reset: 1_793_130_322,
        ..Leg::default()
    };
    (ev, r1, 1_793_130_141_350)
}

/// rp7's table as the verb judged it, the control row apart: R1 failed on its move, the later
/// scenarios not run, the safety rows held (the keychain's unproven: no login directory launched).
fn rp7_rows(ev: &Ev, r1: &Leg) -> Vec<(&'static str, Judged)> {
    let ok = |evidence: &str| Judged {
        verdict: Verdict::Ok,
        evidence: evidence.into(),
    };
    let mut rows = vec![("R1", judge_r1(ev, r1))];
    for id in ["R3", "R4", "R5a", "R5b", "R2"] {
        rows.push((
            id,
            Judged {
                verdict: Verdict::Unproven,
                evidence: "not run".into(),
            },
        ));
    }
    rows.push(("stub", judge_stub(&ev.served)));
    rows.push(("asked", ok("no message")));
    rows.push(("keys", judge_keys(&ev.journal, &[])));
    for id in ["procs", "open", "egress", "owner", "left"] {
        rows.push((id, ok("held")));
    }
    rows.push((
        "keychain",
        Judged {
            verdict: Verdict::Unproven,
            evidence: "3 un-suffixed ask(s), 0 suffixed".into(),
        },
    ));
    rows
}

#[test]
fn the_control_holds_only_where_r1_failed_for_the_route_alone() {
    let (ev, r1, start) = rp7();
    let rows = rp7_rows(&ev, &r1);
    assert_eq!(rows[0].1.verdict, Verdict::Fail, "{}", rows[0].1.evidence);
    let held = judge_control(&ev, start, &r1, &rows);
    assert_eq!(held.verdict, Verdict::Ok, "{}", held.evidence);
    assert!(
        held.evidence
            .contains("account advice: stay (not-a-stored-login)"),
        "{}",
        held.evidence
    );
    let fails = |ev: &Ev, rows: &[(&str, Judged)], says: &str| {
        let j = judge_control(ev, start, &r1, rows);
        assert_eq!(j.verdict, Verdict::Fail, "{says}: {}", j.evidence);
        assert!(j.evidence.contains(says), "{says}: {}", j.evidence);
    };
    // R1 failed for another reason than the route: the host never read the tab as no stored
    // login's (a run that broke, a hang, a judge's slip).
    let mut other = ev.clone();
    other.aterm_log = String::new();
    fails(&other, &rows, "aterm.log holds neither");
    // The wall's record alone stands for it too.
    let mut walled_only = ev.clone();
    walled_only.aterm_log = ATERM_LOG_RP7
        .lines()
        .filter(|l| !l.contains("account advice"))
        .map(|l| format!("{l}\n"))
        .collect();
    assert_eq!(
        judge_control(&walled_only, start, &r1, &rows).verdict,
        Verdict::Ok
    );
    // P1 never served, or never walled: the control never reached its question.
    let mut unserved = ev.clone();
    unserved.served.retain(|s| s.status != 200);
    fails(&unserved, &rows, "P1 was never served on oat:lab-a");
    let mut unwalled = ev.clone();
    unwalled.served.retain(|s| s.status != 429);
    fails(
        &unwalled,
        &rows,
        "oat:lab-a was never walled in R1's window",
    );
    // A move, or a turn on another row, under the control.
    let mut moved = ev.clone();
    moved.journal.push(Said {
        t_ms: 1_793_130_160_000,
        line: "RESTARTED seq=700 rule=account-rotate@v1 relaunch under account lab-b (from the limit its turn ended on; the same conversation)".into(),
    });
    fails(&moved, &rows, "restarted:");
    let mut on_b = ev.clone();
    let mut turn = on_b.served[1].clone();
    turn.auth = AUTH_B.into();
    on_b.served.push(turn);
    fails(&on_b, &rows, "a turn served on oat:lab-b");
    // R1 passing, or a safety row failing, breaks the control.
    let mut passed = rows.clone();
    passed[0].1.verdict = Verdict::Ok;
    fails(&ev, &passed, "R1 did not fail");
    for id in ["procs", "keychain", "asked", "left"] {
        let mut unsafe_rows = rows.clone();
        for (row, j) in &mut unsafe_rows {
            if *row == id {
                j.verdict = Verdict::Fail;
            }
        }
        fails(&ev, &unsafe_rows, &format!("the {id} row is Fail"));
    }
    let mut unproven_open = rows.clone();
    for (row, j) in &mut unproven_open {
        if *row == "open" {
            j.verdict = Verdict::Unproven;
        }
    }
    fails(&ev, &unproven_open, "the open row is Unproven");
}

#[test]
fn the_verbs_exit_is_the_tables_and_under_the_control_the_control_rows() {
    let (ev, r1, start) = rp7();
    let mut rows = rp7_rows(&ev, &r1);
    assert_eq!(verdict_code(false, &rows), 1, "R1's Fail");
    let control = judge_control(&ev, start, &r1, &rows);
    rows.insert(1, ("control", control));
    assert_eq!(verdict_code(true, &rows), 0, "the control held");
    rows[1].1.verdict = Verdict::Fail;
    assert_eq!(verdict_code(true, &rows), 1, "the control broken");
    let passing: Vec<(&str, Judged)> = rows
        .iter()
        .filter(|(id, _)| !["R1", "control"].contains(id))
        .cloned()
        .collect();
    assert_eq!(
        verdict_code(false, &passing),
        0,
        "unproven rows are no Fail"
    );
    assert_eq!(verdict_code(true, &passing), 1, "no control row");
}
