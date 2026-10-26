// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The judges of `claude-login-proof`, each held and failed on excerpts of a real run's evidence
//! (lab `lp-run1`, 2026-10-25, Claude Code 2.1.310 on its default route): tab A
//! `s-a69491f86bf448af665f` (S1), tab B `s-8787e36871885b946278` (S2, S3, S3b).

use super::*;

/// Tab A's journal (S1), its records' `t` and `line`.
const JOURNAL_A: &str = r#"{"t":1792980405997,"line":"EVENT idle seq=345 ▝▝   ▝▝   /private/tmp/at.v1Xx/proj"}
{"t":1792980407334,"line":"EVENT idle seq=389 ⏺ Login expired · Please run /login"}
{"t":1792980407335,"line":"WAITING seq=389 until=2026-10-26T02:16:47Z the login is gone: aterm signs lab-a in again by itself"}
{"t":1792980415936,"line":"WAITING seq=393 until=2026-10-26T02:16:56Z the login is gone: aterm signs lab-a in again by itself"}
"#;

/// Tab B's journal (S2, S3, S3b) up to its `CLEARED` record, which [`journal_b`] adds.
const JOURNAL_B: &str = r#"{"t":1792980408358,"sid":"s-8787e36871885b946278","kind":"event","phase":"idle","seq":378,"line":"EVENT idle seq=378 ⏺ Login expired · Please run /login"}
{"t":1792980408889,"sid":"s-8787e36871885b946278","kind":"typed","phase":"auth-login@v1","seq":378,"summary":"/login","line":"TYPED seq=378 rule=auth-login@v1 /login"}
{"t":1792980409718,"line":"EXCEPTION seq=554 kind=login when=now notice=login-choose Claude Code's login expired, so it cannot answer prompts. => Pick a sign-in method in the tab and finish signing in."}
{"t":1792980409718,"line":"HOLD seq=554 point nothing why=policy"}
{"t":1792980430248,"line":"APPROVED seq=574 the /login aterm typed: Esc, no method picked"}
{"t":1792980430907,"line":"EVENT idle seq=889 ⎿  Login interrupted"}
"#;

/// Tab B's journal whole: [`JOURNAL_B`] and the records that followed the back-out, its
/// `CLEARED` naming `cleared`.
fn journal_b(cleared: &str) -> Vec<Said> {
    let mut j = journal_of(JOURNAL_B);
    j.push(Said {
        t_ms: 1_792_980_430_908,
        line: format!("CLEARED seq=889 exception=login after=21s {cleared}"),
    });
    j.extend(journal_of(
        r#"{"t":1792980430908,"line":"WAITING seq=889 until=2026-10-26T02:17:11Z the login is gone: aterm signs lab-b in again by itself"}
{"t":1792980432511,"line":"ESCALATED seq=1064 attention=OK"}
"#,
    ));
    j
}

/// Tab B's decision ledger.
const DECISIONS_B: &str = r#"{"ts":1792980408357,"sid":"s-8787e36871885b946278","rule_id":"point@v1","decision":"waiting","command":"","reason":"the turn-end policy judged the point (short turns in a row: 1)","box_seq":378}
{"ts":1792980408889,"sid":"s-8787e36871885b946278","rule_id":"auth-login@v1","decision":"typed","command":"/login","reason":"the turn-end policy (short turns in a row: 1)","box_seq":378}
{"ts":1792980409717,"sid":"s-8787e36871885b946278","rule_id":"-","decision":"escalated","command":"","box_seq":554}
{"ts":1792980430248,"sid":"s-8787e36871885b946278","rule_id":"login-back-out@v1","decision":"approved","command":"the /login aterm typed: Esc, no method picked","reason":"","box_seq":574}
{"ts":1792980430787,"sid":"s-8787e36871885b946278","rule_id":"point@v1","decision":"waiting","command":"","box_seq":889}
{"ts":1792980432506,"sid":"s-8787e36871885b946278","rule_id":"-","decision":"escalated","command":"","reason":"a /login you opened: its sign-in method is yours to pick","box_seq":1064}
"#;

/// Tab B's tape's acts, as [`act_line`] kept them.
const ACTS_B: &str = r"1792980408361 send if-human=0 if-gen=1.378 if=^❯\s*$ -- /login
1792980408887 key if-human=0 if-gen=1.393 if=^❯\x{A0}/login\s*$ enter
1792980430248 key if-human=0 if-gen=1.574 if=^\x20{3}❯\x201\.\x20Claude\x20account\x20with\x20subscription\x20·\x20Pro,\x20Max,\x20Team,\x20or\x20Enterprise\s*$ esc
1792980432511 meta set attention owner=supervisor claude other: Login (a /login you opened: its sign-in method is yours to pick)
1792980597491 meta unset attention owner=supervisor
";

/// The accounts' ledger.
const LEDGER: &str = r#"{"account":"lab-a","kind":"auth","sid":"s-a69491f86bf448af665f","t":1792980407,"to":null,"why":"signed-out"}
{"account":"lab-a","kind":"login-ended","sid":"s-a69491f86bf448af665f","t":1792980407,"to":null,"why":""}
{"account":"lab-a","kind":"sign-in","sid":null,"t":1792980407,"to":null,"why":"auto no-route: profile(none) mailbox(none configured) vault(not a member) headless-browser(none installed) hosts(none configured)"}
{"account":"lab-b","kind":"auth","sid":"s-8787e36871885b946278","t":1792980430,"to":null,"why":"signed-out"}
{"account":"lab-b","kind":"login-ended","sid":"s-8787e36871885b946278","t":1792980430,"to":null,"why":""}
{"account":"lab-b","kind":"sign-in","sid":null,"t":1792980430,"to":null,"why":"auto no-route: profile(none) mailbox(none configured) vault(not a member) headless-browser(none installed) hosts(none configured)"}
"#;

/// The window's `messages.log`: tab B's notice posted, then withdrawn at the back-out.
fn messages() -> Vec<Message> {
    let title = format!("{} in tab 2", LOGIN_CHOOSE.title);
    messages_of(&format!(
        "m1\tkind=posted\tid=5\tt=1792980404894\ttag=harness\tsev=info\tglyph=ℹ\ttitle=Autopilot set to cruise\n\
         m1\tkind=posted\tid=10\tt=1792980409718\ttag=harness\tsev=warn\tglyph=⚠\ttitle={title}\tdetail=Pick a sign-in method in the tab and finish signing in.\n\
         m1\tkind=retired\tid=10\thow=withdrawn\tt=1792980430907\ttitle={title}\trep=1\tend=\n"
    ))
}

/// The instance's aterm.log, its lines of the host's looks at the roster: the activation wake
/// armed (its first look), and the first look after the lab laid its roster and added `lab-b`'s
/// row, which the back-out (1792980430248) came before.
const ATERM_LOG: &str = "1792980402.744 15271 INFO aterm_gui::harness_host: harness: the activation wake is armed; every session supervised before it looks at its next idle point
1792980404.894 15271 INFO aterm_gui::messages_host: message 5 harness: Autopilot set to cruise
1792980462.748 15271 INFO aterm_gui::harness_host: harness: the accounts' roster changed: news of the accounts
";

/// S3's marks: the add at 1792980410000, and S3b's Enter.
const S3: (i64, i64) = (1_792_980_410_000, 1_792_980_432_100);
/// S3b's: the person's Enter, and the window's end.
const S3B: (i64, i64) = (1_792_980_432_100, 1_792_980_597_400);

fn ledger() -> Vec<Booked> {
    ledger_of(LEDGER)
}

fn acts_b() -> Vec<Act> {
    acts_of(ACTS_B)
}

fn with_act(acts: &[Act], t_ms: i64, line: &str) -> Vec<Act> {
    let mut v = acts.to_vec();
    v.push(Act {
        t_ms,
        line: line.to_string(),
    });
    v.sort_by_key(|a| a.t_ms);
    v
}

#[test]
fn the_evidence_reads_the_harness_records_it_is_kept_in() {
    let a = journal_of(JOURNAL_A);
    assert_eq!(a.len(), 4);
    assert_eq!(a[2].t_ms, 1_792_980_407_335);
    assert!(a[2].line.starts_with("WAITING seq=389 "));
    assert_eq!(
        journal_of("not json\n{\"line\":\"no t\"}\n").len(),
        0,
        "a line that is no record is skipped"
    );
    let d = decisions_of(DECISIONS_B);
    assert_eq!(d.len(), 6);
    assert_eq!(
        (d[3].t_ms, d[3].rule.as_str(), d[3].decision.as_str()),
        (1_792_980_430_248, RULE_LOGIN_BACK_OUT, "approved")
    );
    let acts = acts_b();
    assert_eq!(acts.len(), 5);
    assert!(is_login_send(&acts[0].line) && is_login_enter(&acts[1].line));
    assert!(is_fenced_esc(&acts[2].line));
    assert!(
        acts.iter().filter(|a| is_input(&a.line)).count() == 3,
        "the badge's meta writes type nothing"
    );
    let words: Vec<String> = ["@s-8787e36871885b946278", "key", "if-human=0", "esc"]
        .map(String::from)
        .to_vec();
    assert_eq!(
        act_line(7, &words),
        "7 key if-human=0 esc",
        "the tab's own @sid is dropped"
    );
    assert_eq!(acts_of(&act_line(7, &words))[0].line, "key if-human=0 esc");
    let l = ledger();
    assert_eq!(l.len(), 6);
    assert_eq!(
        (l[2].account.as_str(), l[2].kind.as_str()),
        ("lab-a", "sign-in")
    );
    let m = messages();
    assert_eq!(m.len(), 3);
    assert_eq!(
        (m[2].kind.as_str(), m[2].t_ms),
        ("retired", 1_792_980_430_907)
    );
    assert!(names("signs lab-a in", "lab-a"));
    assert!(
        !names("signs lab-ab in", "lab-a") && !names("signs xlab-a in", "lab-a"),
        "a label is a word"
    );
}

#[test]
fn s1_holds_on_a_roster_rows_wait_and_fails_on_a_login_typed_or_told() {
    let journal = journal_of(JOURNAL_A);
    let held = judge_s1(&journal, &[], &ledger(), LABEL_A);
    assert_eq!(held.verdict, Verdict::Ok, "{}", held.evidence);
    let badge = with_act(
        &[],
        1_792_980_408_000,
        "meta set attention owner=supervisor x",
    );
    assert_eq!(
        judge_s1(&journal, &badge, &ledger(), LABEL_A).verdict,
        Verdict::Ok,
        "a badge types nothing"
    );

    let typed = with_act(
        &[],
        1_792_980_408_000,
        r"send if-human=0 if=^❯\s*$ -- /login",
    );
    let mut told = journal.clone();
    told.push(Said {
        t_ms: 1_792_980_409_000,
        line: "TYPED seq=400 rule=auth-login@v1 /login".into(),
    });
    let mut escalated = journal.clone();
    escalated.push(Said {
        t_ms: 1_792_980_409_000,
        line: "EXCEPTION seq=400 kind=login when=now notice=login-choose x".into(),
    });
    let no_route_missing: Vec<Booked> = ledger()
        .into_iter()
        .filter(|b| !(b.account == LABEL_A && b.kind == "sign-in"))
        .collect();
    for (case, journal, acts, ledger, label) in [
        ("an input", journal.clone(), typed, ledger(), LABEL_A),
        ("a /login typed", told, Vec::new(), ledger(), LABEL_A),
        ("told", escalated, Vec::new(), ledger(), LABEL_A),
        (
            "no sign-in booked",
            journal.clone(),
            Vec::new(),
            no_route_missing,
            LABEL_A,
        ),
        (
            "another row's wait",
            journal.clone(),
            Vec::new(),
            ledger(),
            LABEL_B,
        ),
    ] {
        let j = judge_s1(&journal, &acts, &ledger, label);
        assert_eq!(j.verdict, Verdict::Fail, "{case}: {}", j.evidence);
    }
}

#[test]
fn s2_holds_on_one_fenced_login_and_enter_and_fails_on_a_key_in_the_picker() {
    let journal = journal_b(LOGIN_BACKED_OUT);
    let held = judge_s2(&journal, &acts_b(), &messages(), S3.0);
    assert_eq!(held.verdict, Verdict::Ok, "{}", held.evidence);

    let picked = with_act(
        &acts_b(),
        1_792_980_409_900,
        r"key if-human=0 if-gen=1.560 if=^\x20{3}❯\x201\.\x20Claude\x20account\s*$ enter",
    );
    let unfenced = with_act(&acts_b()[1..], 1_792_980_408_361, "send -- /login");
    let untold: Vec<Said> = journal
        .iter()
        .filter(|s| !s.line.starts_with("EXCEPTION "))
        .cloned()
        .collect();
    let quiet: Vec<Message> = messages()
        .into_iter()
        .filter(|m| m.kind != "posted" || !m.title.starts_with(LOGIN_CHOOSE.title))
        .collect();
    for (case, journal, acts, messages, to) in [
        (
            "an Enter in the picker",
            journal.clone(),
            picked,
            messages(),
            S3.0,
        ),
        (
            "an unfenced /login",
            journal.clone(),
            unfenced,
            messages(),
            S3.0,
        ),
        ("no exception told", untold, acts_b(), messages(), S3.0),
        ("no notice posted", journal.clone(), acts_b(), quiet, S3.0),
        (
            "cut before the Enter",
            journal.clone(),
            acts_b(),
            messages(),
            1_792_980_408_500,
        ),
    ] {
        let j = judge_s2(&journal, &acts, &messages, to);
        assert_eq!(j.verdict, Verdict::Fail, "{case}: {}", j.evidence);
    }
}

#[test]
fn the_hosts_looks_at_the_roster_are_read_off_its_log_and_place_the_add() {
    assert_eq!(looks_of(ATERM_LOG), [1_792_980_462_748]);
    assert_eq!(log_times(ATERM_LOG, WAKE_ARMED), [1_792_980_402_744]);
    assert_eq!(
        log_times(
            "no time here harness: the activation wake is armed\n",
            WAKE_ARMED
        ),
        Vec::<i64>::new(),
        "a line whose time does not read is skipped"
    );
    let armed = 1_792_980_402_744;
    let only_armed = ATERM_LOG.lines().next().unwrap();
    assert_eq!(
        add_wait(only_armed, armed + 7_000),
        None,
        "7 s after the arming, the first look: at once"
    );
    assert_eq!(
        add_wait(only_armed, armed + 30_000),
        Some(Duration::from_secs(30) + LOOK_SETTLE),
        "30 s after it: just past the next look"
    );
    assert_eq!(
        add_wait(only_armed, armed + 125_000),
        None,
        "5 s past the third look"
    );
    assert_eq!(
        add_wait(ATERM_LOG, 1_792_980_462_748 + 10_000),
        None,
        "the latest look logged counts"
    );
    assert_eq!(add_wait("", armed), None, "no look logged: at once");
}

#[test]
fn s3_holds_on_the_back_out_before_the_hosts_look_and_fails_without_it() {
    let decisions = decisions_of(DECISIONS_B);
    let looks = looks_of(ATERM_LOG);
    let judge = |journal: &[Said], acts: &[Act], decisions: &[Decided], looks: &[i64], marks| {
        judge_s3(
            (journal, decisions, acts),
            (&ledger(), &messages(), looks),
            LABEL_B,
            marks,
        )
    };
    let journal = journal_b(LOGIN_BACKED_OUT);
    let held = judge(&journal, &acts_b(), &decisions, &looks, S3);
    assert_eq!(held.verdict, Verdict::Ok, "{}", held.evidence);

    let generic = journal_b("`Login successful` on the screen, or the agent working");
    let unwaited: Vec<Said> = journal
        .iter()
        .filter(|s| !(s.line.starts_with("WAITING ") && names(&s.line, LABEL_B)))
        .cloned()
        .collect();
    let mut retyped = journal.clone();
    retyped.push(Said {
        t_ms: 1_792_980_431_000,
        line: "TYPED seq=900 rule=auth-login@v1 /login".into(),
    });
    let twice = with_act(
        &acts_b(),
        1_792_980_431_000,
        r"send if-human=0 if=^❯\s*$ -- /login",
    );
    let stray = with_act(
        &acts_b(),
        1_792_980_475_000,
        r"key if-human=0 if-gen=1.900 if=^❯\s*$ enter",
    );
    let mut approved_twice = decisions.clone();
    approved_twice.push(Decided {
        t_ms: 1_792_980_431_500,
        rule: RULE_LOGIN_BACK_OUT.into(),
        decision: "approved".into(),
    });
    let look_first = [1_792_980_420_000];
    let late_s3b = (S3.0, 1_792_980_480_000);
    for (case, journal, acts, decisions, looks, marks) in [
        (
            "the CLEARED line names no back-out",
            generic,
            acts_b(),
            decisions.clone(),
            looks.clone(),
            S3,
        ),
        (
            "no WAITING naming the row after the Esc",
            unwaited,
            acts_b(),
            decisions.clone(),
            looks.clone(),
            S3,
        ),
        (
            "a TYPED /login after the add, no input beside the Esc",
            retyped,
            acts_b(),
            decisions.clone(),
            looks.clone(),
            S3,
        ),
        (
            "a second input, a /login sent",
            journal.clone(),
            twice,
            decisions.clone(),
            looks.clone(),
            S3,
        ),
        (
            "a stray key more than a minute after the add, before S3b",
            journal.clone(),
            stray,
            decisions.clone(),
            looks.clone(),
            late_s3b,
        ),
        (
            "a second back-out approved before S3b",
            journal.clone(),
            acts_b(),
            approved_twice,
            looks.clone(),
            S3,
        ),
        (
            "the Esc after the host's own look at the roster (no wake)",
            journal.clone(),
            acts_b(),
            decisions.clone(),
            look_first.to_vec(),
            S3,
        ),
        (
            "no look of the host's logged after the add",
            journal.clone(),
            acts_b(),
            decisions.clone(),
            Vec::new(),
            S3,
        ),
        (
            "the Esc only after S3b's start",
            journal.clone(),
            acts_b(),
            decisions.clone(),
            looks.clone(),
            (S3.0, 1_792_980_430_000),
        ),
    ] {
        let j = judge(&journal, &acts, &decisions, &looks, marks);
        assert_eq!(j.verdict, Verdict::Fail, "{case}: {}", j.evidence);
    }
    let kept: Vec<Message> = messages()
        .into_iter()
        .filter(|m| m.kind != "retired")
        .collect();
    let j = judge_s3(
        (&journal, &decisions, &acts_b()),
        (&ledger(), &kept, &looks),
        LABEL_B,
        S3,
    );
    assert_eq!(
        j.verdict,
        Verdict::Fail,
        "the notice not withdrawn: {}",
        j.evidence
    );
    let mut unbooked = ledger();
    unbooked.retain(|b| !(b.account == LABEL_B && b.kind == "sign-in"));
    let j = judge_s3(
        (&journal, &decisions, &acts_b()),
        (&unbooked, &messages(), &looks),
        LABEL_B,
        S3,
    );
    assert_eq!(
        j.verdict,
        Verdict::Fail,
        "no sign-in booked: {}",
        j.evidence
    );
}

#[test]
fn s3b_holds_on_a_picker_left_alone_past_the_grace_and_fails_on_a_key_or_a_short_watch() {
    let journal = journal_b(LOGIN_BACKED_OUT);
    let decisions = decisions_of(DECISIONS_B);
    let grace = Duration::from_secs(150);
    let watched = Duration::from_secs(165);
    let held = judge_s3b(
        (&journal, &decisions, &acts_b()),
        &messages(),
        S3B,
        (watched, grace),
    );
    assert_eq!(held.verdict, Verdict::Ok, "{}", held.evidence);

    let esc = with_act(
        &acts_b(),
        1_792_980_590_000,
        r"key if-human=1 if=^\x20{3}❯\s*$ esc",
    );
    let mut approved = decisions.clone();
    approved.push(Decided {
        t_ms: 1_792_980_590_000,
        rule: RULE_LOGIN_BACK_OUT.into(),
        decision: "approved".into(),
    });
    let unescalated: Vec<Decided> = decisions
        .iter()
        .filter(|d| d.t_ms < S3B.0)
        .cloned()
        .collect();
    let mut noticed = messages();
    noticed.push(Message {
        kind: "posted".into(),
        t_ms: 1_792_980_433_000,
        title: format!("{} in tab 2", LOGIN_CHOOSE.title),
    });
    let mut retyped = journal.clone();
    retyped.push(Said {
        t_ms: 1_792_980_500_000,
        line: "TYPED seq=1100 rule=auth-login@v1 /login".into(),
    });
    let j = judge_s3b(
        (&retyped, &decisions, &acts_b()),
        &messages(),
        S3B,
        (watched, grace),
    );
    assert_eq!(
        j.verdict,
        Verdict::Fail,
        "a TYPED record in the window, the acts as they were: {}",
        j.evidence
    );
    for (case, acts, decisions, messages, watched) in [
        (
            "an Esc at the person's picker",
            esc,
            decisions.clone(),
            messages(),
            watched,
        ),
        (
            "a back-out approved",
            acts_b(),
            approved,
            messages(),
            watched,
        ),
        ("not escalated", acts_b(), unescalated, messages(), watched),
        (
            "a notice posted for it",
            acts_b(),
            decisions.clone(),
            noticed,
            watched,
        ),
        (
            "watched under the grace",
            acts_b(),
            decisions.clone(),
            messages(),
            Duration::from_secs(100),
        ),
    ] {
        let j = judge_s3b(
            (&journal, &decisions, &acts),
            &messages,
            S3B,
            (watched, grace),
        );
        assert_eq!(j.verdict, Verdict::Fail, "{case}: {}", j.evidence);
    }
}

#[test]
fn s3b_grace_is_the_supervisors_own_and_moves_with_it() {
    let mut cfg = SupervisorConfig::default();
    let take = TurnEndTiming::default().take_within;
    assert_eq!(
        s3b_grace(&cfg),
        Duration::from_secs(u64::from(cfg.human_grace_s)) + take
    );
    cfg.human_grace_s += 60;
    assert_eq!(
        s3b_grace(&cfg),
        Duration::from_secs(u64::from(cfg.human_grace_s)) + take,
        "a longer grace is watched longer"
    );
}

#[test]
fn the_safety_judges_fail_on_a_sighting_an_external_socket_or_an_open_home() {
    assert_eq!(judge_procs(&[], true, 630).verdict, Verdict::Ok);
    assert_eq!(
        judge_procs(&[], false, 630).verdict,
        Verdict::Unproven,
        "a watcher whose control went unseen proves nothing"
    );
    assert_eq!(
        judge_procs(
            &["auth login: pid 9 /c/claude auth login".into()],
            true,
            630
        )
        .verdict,
        Verdict::Fail
    );
    let sockets = "19:06:42 aterm-lab[15029] TCP listen 127.0.0.1:57232\n\
                   19:06:42 aterm-lab[15029] TCP listen 127.0.0.1:57233\n";
    assert_eq!(judge_egress(sockets, "").verdict, Verdict::Ok);
    assert_eq!(judge_egress("", "").verdict, Verdict::Unproven);
    let out = "19:07:01 claude[2] TCP conn 160.79.104.10:443  EXTERNAL\n";
    assert_eq!(judge_egress(out, "").verdict, Verdict::Fail);
    assert_eq!(
        judge_egress(sockets, &format!("19:07:01 EXTERNAL {}", out.trim())).verdict,
        Verdict::Fail
    );
    let homes = vec!["/Users//example".to_string()];
    let jail = "(deny file-write* (subpath \"/Users//example\"))\n\
                (deny file-write* (subpath \"/private/tmp/at.lab-501/aterm-32e6a5a1/runs/lp-run1\"))\n";
    assert_eq!(judge_owner(jail, &homes).verdict, Verdict::Ok);
    assert_eq!(
        judge_owner(jail, &["/Users//other".to_string()]).verdict,
        Verdict::Fail
    );
    assert_eq!(
        judge_owner(jail, &[]).verdict,
        Verdict::Fail,
        "no home guarded"
    );
}

/// The final run's `security.log` (lab `lp-final`, 2026-10-25), the account masked as `down`
/// keeps it: each tab's launch asked for its own login directory's suffixed items.
const SECURITY: &str = r#"20:00:41 find-generic-password account="<user>" service="Claude Code-credentials-5df98271" via=argv pid=35939 ppid=35937
20:00:41 find-generic-password account="<user>" service="Claude Code-5df98271" via=argv pid=35940 ppid=35937
20:00:44 find-generic-password account="<user>" service="Claude Code-credentials-24e767c7" via=argv pid=36044 ppid=36040
"#;

#[test]
fn the_keychain_judge_fails_on_the_owners_own_items_and_proves_nothing_unasked() {
    let held = judge_keychain(SECURITY);
    assert_eq!(held.verdict, Verdict::Ok, "{}", held.evidence);
    for owner in OWNER_ITEMS {
        let asked = format!(
            "{SECURITY}20:00:45 find-generic-password account=\"<user>\" service=\"{owner}\" via=argv pid=9 ppid=8\n"
        );
        let j = judge_keychain(&asked);
        assert_eq!(j.verdict, Verdict::Fail, "{owner}: {}", j.evidence);
    }
    assert_eq!(
        judge_keychain("").verdict,
        Verdict::Unproven,
        "a tripwire never reached proves nothing"
    );
    assert_eq!(
        judge_keychain("20:00:41 find-generic-password via=argv\n").verdict,
        Verdict::Unproven,
        "a line naming no item is no lookup"
    );
}

#[test]
fn the_watcher_suspects_auth_login_and_opens_and_nothing_else() {
    let claude = "/Users//o/Library/Application Support/aterm/pkg/store/claude/1000002000001000310/bin/claude";
    assert_eq!(suspect(claude), None);
    assert_eq!(suspect(&format!("{claude} auth login")), Some("auth login"));
    assert_eq!(
        suspect(&format!(
            "/bin/sh -c /bin/sleep 2; : {PROC_CONTROL} auth login"
        )),
        Some("auth login"),
        "the watcher's own control"
    );
    assert_eq!(
        suspect("/usr/bin/open https://claude.ai/oauth/authorize"),
        Some("open")
    );
    assert_eq!(suspect("/r/shim/open https://claude.ai"), Some("open"));
    assert_eq!(
        suspect("/Applications/Safari.app/Contents/MacOS/Safari https://claude.ai"),
        Some("open")
    );
    assert_eq!(suspect("sh /r/launch/s-8787e36871885b946278.sh"), None);
    assert_eq!(
        suspect("env -u CLAUDE_CONFIG_DIR BROWSER=/r/shim/open /c/claude"),
        None,
        "a word naming the shim is no launch of it"
    );
    assert_eq!(suspect("/x/aterm harness accounts add lab-b"), None);
}
