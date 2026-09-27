// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The supervisor's hold on a worker that stopped reading its input
// (`stall.rs`, the incident of 2026-09-24), over the scripted server: the
// badge withdrawn, the manager told once, nothing pressed or typed while the
// stall stands, the point decided again once it lifts, a stall the `status`
// in hand reports acted on before anything is pressed or asked, the window's
// host (no manager) held as a `watch` is — and a host that publishes no
// `input=` seeing at most one `status` more than it saw before, the read that
// learns so. Included from `run.rs`'s tests (`mod stall_hold`), so the `Mock`
// and the helpers there are in scope.

/// The requests that write into the worker: a key, text, a turn.
fn writes(m: &Mock) -> Vec<&String> {
    m.requests
        .iter()
        .filter(|r| r.starts_with("key") || r.starts_with("send") || r.starts_with("turn"))
        .collect()
}

/// The badge and mail requests, in order.
fn badge_acts(m: &Mock) -> Vec<String> {
    m.requests
        .iter()
        .filter(|r| {
            r.starts_with("meta set") || r.starts_with("meta unset") || r.starts_with("post")
        })
        .cloned()
        .collect()
}

/// The journal's lines, as printed.
fn journal_lines(path: &Path) -> Vec<String> {
    journal_records(path)
        .0
        .into_iter()
        .map(|r| r.line)
        .collect()
}

/// The loop's `watch` over `m` with a manager to mail and a journal: the
/// printed lines, the exit code and the journal's lines.
fn watch_journaled(m: &mut Mock, tag: &str) -> (Vec<String>, u8, Vec<String>) {
    let (dir, path) = journal_file(tag);
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..auto(30, None)
    };
    let (lines, code) = watch_lines_with(m, &opts, |s| {
        s.set_manager(Some("@s-9".to_string()));
    });
    let journal = journal_lines(&path);
    let _ = std::fs::remove_dir_all(&dir);
    (lines, code, journal)
}

/// A scripted worker showing a box no rule approves (a Read of
/// `~/.ssh/config`) that never leaves the screen, three post replies in
/// hand.
fn box_that_sits() -> Mock {
    let mut m = Mock::new(true, vec![busy_screen(), read_box()]);
    m.vanish_after = Some(4);
    m.verb_replies.insert(
        "post",
        VecDeque::from([
            ok("OK 7 off=91\n"),
            ok("OK 8 off=92\n"),
            ok("OK 9 off=93\n"),
        ]),
    );
    m
}

/// The end every one of these runs must reach: the session gone under the
/// wait — never a refusal read as the loop's own fault (`Fail::Hard`
/// "status failed", "key … failed").
fn ended_with_the_session(lines: &[String], code: u8) {
    let last = lines.last().expect("an end line");
    assert!(
        last.starts_with("EXIT session gone (") && last.ends_with("ERR exited)"),
        "{lines:?}"
    );
    assert_eq!(code, 1);
}

/// THE INCIDENT, as the loop met it: a box no rule approves, escalated, and
/// then the worker stops reading its input with the box still on its screen,
/// so `await gone <the box>` never latches. The probe after the first step
/// that runs out reads `input=stalled`: the loop's badge is unset — the
/// server's own `aterm` entry says "frozen" over it — ONE `kind=ask` tells
/// the manager, `FROZEN …` is journaled, and nothing is pressed, typed or
/// escalated again while it stands; every later step is probed again and
/// says nothing new.
#[test]
fn a_frozen_worker_is_held_its_badge_withdrawn_and_the_manager_told_once() {
    let mut m = box_that_sits();
    // Read 1 is the box's program, read 2 the escalation's fabric: the
    // worker reads its input until then, and from the wait's first probe on
    // it does not.
    m.input_after = vec![(2, INPUT_STALLED)];
    let (lines, code, journal) = watch_journaled(&mut m, "stall-held");
    assert!(writes(&m).is_empty(), "{:?}", m.requests);
    let acts = badge_acts(&m);
    assert_eq!(acts.len(), 4, "{acts:#?}");
    assert!(
        acts[0].starts_with("meta set attention owner=supervisor claude read: ~/.ssh/config ("),
        "{acts:#?}"
    );
    assert!(acts[1].starts_with("post to=@s-9 kind=ask --wait=0 claude read: "));
    assert_eq!(acts[2], "meta unset attention owner=supervisor");
    assert_eq!(
        acts[3],
        "post to=@s-9 kind=ask --wait=0 claude frozen: not reading input for 2m41s (1 B \
         queued, rss 38.8 GB); pressing nothing until it reads again \u{2014} restart it: \
         signal term (signal kill if it survives), then claude --continue"
    );
    let probes = m.requests.iter().filter(|r| *r == "status").count();
    assert!(probes >= 5, "a probe per step: {:?}", m.requests);
    let frozen: Vec<&String> = journal.iter().filter(|l| l.starts_with("FROZEN")).collect();
    assert_eq!(frozen.len(), 1, "{journal:#?}");
    assert!(
        frozen[0].ends_with(
            " input=stalled wait_ms=161000 bytes=1 rss_mb=39731 attention=OK mail=OK 8 off=92"
        ),
        "{}",
        frozen[0]
    );
    assert!(!journal.iter().any(|l| l.starts_with("THAWED")));
    let prompts = lines
        .iter()
        .filter(|l| l.starts_with("EVENT prompt "))
        .count();
    assert_eq!(prompts, 1, "{lines:?}");
    ended_with_the_session(&lines, code);
}

/// The fabric is not connected: the badge still goes, and the mail is
/// journaled skipped, never posted (the rule every escalation's mail keeps).
#[test]
fn a_frozen_worker_with_no_fabric_is_told_to_no_one_by_mail() {
    let mut m = box_that_sits();
    m.fabric = "absent";
    m.input_after = vec![(2, INPUT_STALLED)];
    let (lines, code, journal) = watch_journaled(&mut m, "stall-nofabric");
    assert!(writes(&m).is_empty(), "{:?}", m.requests);
    assert!(!m.requests.iter().any(|r| r.starts_with("post")));
    assert_eq!(count(&m, "meta unset attention owner=supervisor"), 1);
    let frozen = journal
        .iter()
        .find(|l| l.starts_with("FROZEN"))
        .expect("FROZEN");
    assert!(
        frozen.ends_with("attention=OK mail=skipped: no fabric (fabric=absent)"),
        "{frozen}"
    );
    ended_with_the_session(&lines, code);
}

/// The stall lifts — `status` reads `input=clear` again — while the box is
/// still on the screen: `THAWED …` is journaled and the box, a point the
/// loop had handed over, is decided afresh: a review point again, escalated
/// again, badge and ask. Nothing was pressed through the stall.
#[test]
fn a_thawed_worker_has_its_point_decided_again() {
    let mut m = box_that_sits();
    // As in the incident: reading its input through the box's escalation
    // (reads 1 and 2), stalled from the wait's first probe (read 3), clear
    // again from its third (read 5) — and the next look thaws.
    m.input_after = vec![(2, INPUT_STALLED), (4, INPUT_CLEAR)];
    let (lines, code, journal) = watch_journaled(&mut m, "stall-thawed");
    assert!(writes(&m).is_empty(), "{:?}", m.requests);
    let acts = badge_acts(&m);
    assert_eq!(acts.len(), 6, "{acts:#?}");
    assert!(acts[0].starts_with("meta set attention owner=supervisor claude read: "));
    assert!(acts[1].starts_with("post to=@s-9 kind=ask --wait=0 claude read: "));
    assert_eq!(acts[2], "meta unset attention owner=supervisor");
    assert!(acts[3].starts_with("post to=@s-9 kind=ask --wait=0 claude frozen: "));
    assert_eq!(acts[4], acts[0], "the same box, escalated again");
    assert_eq!(acts[5], acts[1]);
    let at = |word: &str| journal.iter().position(|l| l.starts_with(word));
    let (frozen, thawed) = (at("FROZEN"), at("THAWED"));
    assert!(
        frozen.is_some() && thawed.is_some() && frozen < thawed,
        "{journal:#?}"
    );
    assert!(
        journal[thawed.expect("thawed")].ends_with(" input=clear"),
        "{journal:#?}"
    );
    let prompts = lines
        .iter()
        .filter(|l| l.starts_with("EVENT prompt "))
        .count();
    assert_eq!(prompts, 2, "the box is a review point again: {lines:?}");
    ended_with_the_session(&lines, code);
}

/// U1: the loop tells its host the stall it holds, and that it lifted
/// ([`IdleHost::stalled`]) — the host's word on an agent that exits while
/// one is held: the stall's remedy ended it, and it is relaunched on its
/// conversation. NEGATIVE CONTROL: a worker that never stalls tells the
/// host nothing.
#[test]
fn the_host_is_told_the_stall_it_holds_and_that_it_lifted() {
    #[derive(Debug, Default)]
    struct Told(std::sync::Mutex<Vec<bool>>);
    impl IdleHost for Told {
        fn wants(&self) -> bool {
            false
        }
        fn at_idle(&self) -> Option<String> {
            None
        }
        fn owns_turn_end(&self) -> bool {
            false
        }
        fn stalled(&self, held: bool) {
            self.0.lock().unwrap().push(held);
        }
    }
    for (stalls, want) in [(true, vec![true, false]), (false, vec![])] {
        let told = Arc::new(Told::default());
        let mut m = box_that_sits();
        if stalls {
            m.input_after = vec![(2, INPUT_STALLED), (4, INPUT_CLEAR)];
        }
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&told) as Arc<dyn IdleHost>),
            ..auto(30, None)
        };
        let _ = watch_lines_with(&mut m, &opts, |s| {
            s.set_manager(Some("@s-9".to_string()));
        });
        assert_eq!(*told.0.lock().unwrap(), want, "{:#?}", m.requests);
    }
}

/// A press the server refused `ERR busy input-unread …` (the gate fires at
/// 1 s, the stall is published at 10 s): the loop backs off as for any
/// `ERR busy`, and its next look reads `status` BEFORE it presses again —
/// here the stall has been published by then, so it holds: one press, never
/// a second.
#[test]
fn a_press_refused_input_unread_reads_status_before_it_presses_again() {
    let mut m = Mock::new(true, vec![busy_screen(), bash_one_row()]);
    m.vanish_after = Some(4);
    m.key_replies.push_back(err(
        "busy input-unread bytes=1 wait_ms=1500 input=pending (the program has not read \
         input queued 1.5s ago: a key sent now is read after it, against a screen the program \
         has not drawn — retry later, restart a frozen program with signal term, or lead \
         send/key with unread=ok to queue anyway)",
    ));
    // Read 1 is the box's program; from read 2 — the look's own read after
    // the back-off — the stall is published.
    m.input_after = vec![(1, INPUT_STALLED)];
    let (lines, code, journal) = watch_journaled(&mut m, "stall-refused");
    let presses = writes(&m);
    assert_eq!(presses.len(), 1, "{:?}", m.requests);
    let refused = journal
        .iter()
        .find(|l| l.starts_with("REFUSED"))
        .expect("REFUSED");
    assert!(refused.contains("ERR busy input-unread"), "{refused}");
    // After the press: the back-off's wait, the next look's read of the
    // screen, and then `status` — before anything else is decided.
    let key_at = m
        .requests
        .iter()
        .position(|r| r.starts_with("key"))
        .expect("the press");
    let after: Vec<&str> = m.requests[key_at + 1..]
        .iter()
        .map(String::as_str)
        .collect();
    let read_at = after
        .iter()
        .position(|r| r.starts_with("text --json"))
        .expect("the next look's read");
    assert_eq!(after.get(read_at + 1), Some(&"status"), "{after:#?}");
    assert!(
        journal.iter().any(|l| l.starts_with("FROZEN")),
        "{journal:#?}"
    );
    ended_with_the_session(&lines, code);
}

/// NEGATIVE CONTROL: a host that publishes no `input=` — an aterm from
/// before 2026-09-24 — or one that cannot measure it (`input=-`: not macOS,
/// not a tty) sees exactly the requests it saw before: `status` only where
/// the box and the escalation read it, no probe on any step, no hold.
#[test]
fn a_host_that_does_not_measure_input_sees_the_requests_it_always_saw() {
    for input in ["", "input=- input_bytes=- input_wait_ms=- fg_rss_mb=-"] {
        let mut m = box_that_sits();
        m.input = input;
        let (lines, code, journal) = watch_journaled(&mut m, "stall-none");
        let find = |prefix: &str| {
            m.requests
                .iter()
                .find(|r| r.starts_with(prefix))
                .cloned()
                .unwrap_or_else(|| panic!("no {prefix}: {:#?}", m.requests))
        };
        let set = find("meta set attention owner=supervisor claude read: ");
        let post = find("post to=@s-9 kind=ask --wait=0 claude read: ");
        let anchor = "await gone ^\\x20{3}~/\\.ssh/config\\s*$ timeout 20000";
        assert_eq!(
            m.requests,
            [
                "meta",
                "await gone esc.to.interrupt timeout 20000",
                "text --json tail=40",
                "await gone esc.to.interrupt timeout 20000",
                "text --json tail=40",
                "status",
                set.as_str(),
                "status",
                post.as_str(),
                anchor,
                anchor,
                anchor,
                anchor,
                anchor,
            ],
            "input={input:?}"
        );
        assert!(
            !journal
                .iter()
                .any(|l| l.starts_with("FROZEN") || l.starts_with("THAWED")),
            "{journal:#?}"
        );
        ended_with_the_session(&lines, code);
    }
}

/// Review finding on S8 (2026-09-25): a loop that starts, restarts or
/// reconnects over a worker ALREADY frozen read `input=stalled` in the very
/// `status` that named the box's program — and pressed the box, or badged it
/// "answer this box" and asked the manager to, the incident's misleading
/// badge, withdrawn only by the next wait step 20 s later with a second mail.
/// The status in hand is acted on now: the look goes round, and its next
/// read holds before anything is pressed or badged — for a box a rule
/// approves (a `git log`) and for one none does (a Read of `~/.ssh/config`).
#[test]
fn a_worker_already_stalled_when_the_loop_looks_is_neither_pressed_nor_badged() {
    for (screen, tag) in [
        (bash_one_row(), "stall-first-press"),
        (read_box(), "stall-first-box"),
    ] {
        let mut m = Mock::new(true, vec![busy_screen(), screen]);
        m.vanish_after = Some(4);
        m.verb_replies
            .insert("post", VecDeque::from([ok("OK 7 off=91\n")]));
        m.input = INPUT_STALLED;
        let (lines, code, journal) = watch_journaled(&mut m, tag);
        assert!(writes(&m).is_empty(), "{tag}: {:?}", m.requests);
        assert_eq!(
            badge_acts(&m),
            [
                "post to=@s-9 kind=ask --wait=0 claude frozen: not reading input for 2m41s (1 B \
              queued, rss 38.8 GB); pressing nothing until it reads again \u{2014} restart it: \
              signal term (signal kill if it survives), then claude --continue"
            ],
            "{tag}: {:#?}",
            m.requests
        );
        assert!(
            !journal
                .iter()
                .any(|l| l.starts_with("APPROVED") || l.starts_with("ESCALATED")),
            "{tag}: {journal:#?}"
        );
        let frozen: Vec<&String> = journal.iter().filter(|l| l.starts_with("FROZEN")).collect();
        assert_eq!(frozen.len(), 1, "{tag}: {journal:#?}");
        assert!(
            frozen[0].ends_with(" attention=none mail=OK 7 off=91"),
            "{tag}: {}",
            frozen[0]
        );
        assert!(
            !lines.iter().any(|l| l.starts_with("EVENT prompt ")),
            "{tag}: {lines:?}"
        );
        ended_with_the_session(&lines, code);
    }
}

/// The same, for a point with no `status` read before its escalation (a
/// question the worker asked): the badge goes up before the escalation's own
/// `status` reads the stall, but that read is acted on — no ask to answer it
/// is mailed, and the hold withdraws the badge before the wait's first step,
/// not 20 s after it. One mail, the frozen one.
#[test]
fn a_stall_the_escalations_own_read_reports_withholds_the_ask_and_holds_at_once() {
    let mut m = Mock::new(true, vec![busy_screen(), question_screen()]);
    m.vanish_after = Some(3);
    m.verb_replies
        .insert("post", VecDeque::from([ok("OK 7 off=91\n")]));
    m.input = INPUT_STALLED;
    let (lines, code, journal) = watch_journaled(&mut m, "stall-first-question");
    assert!(writes(&m).is_empty(), "{:?}", m.requests);
    let acts = badge_acts(&m);
    assert_eq!(acts.len(), 3, "{acts:#?}");
    assert!(
        acts[0].starts_with("meta set attention owner=supervisor claude "),
        "{acts:#?}"
    );
    assert_eq!(acts[1], "meta unset attention owner=supervisor");
    assert!(
        acts[2].starts_with("post to=@s-9 kind=ask --wait=0 claude frozen: "),
        "{acts:#?}"
    );
    // The unset comes before the wait's first step.
    let unset_at = m
        .requests
        .iter()
        .position(|r| r.starts_with("meta unset"))
        .expect("the unset");
    assert!(
        !m.requests[..unset_at]
            .iter()
            .any(|r| r.starts_with("await seq")),
        "{:#?}",
        m.requests
    );
    let escalated = journal
        .iter()
        .find(|l| l.starts_with("ESCALATED"))
        .expect("ESCALATED");
    assert!(
        escalated.ends_with(
            " attention=OK mail=skipped: the same status says the worker is not reading its input"
        ),
        "{escalated}"
    );
    let frozen = journal
        .iter()
        .find(|l| l.starts_with("FROZEN"))
        .expect("FROZEN");
    assert!(
        frozen.ends_with(" attention=OK mail=OK 7 off=91"),
        "{frozen}"
    );
    ended_with_the_session(&lines, code);
}

/// Stalled from the loop's first read, reading again later: the box sat
/// through the stall undecided — nothing pressed, no badge, no ask — and is
/// decided once, after `THAWED`: escalated then, a review point then.
#[test]
fn a_worker_stalled_from_the_start_has_its_point_decided_once_it_reads_again() {
    let mut m = box_that_sits();
    // Read 1 (the box's program) and the hold's reads 2-3 see the stall;
    // the wait's second probe (read 4) sees it lift.
    m.input = INPUT_STALLED;
    m.input_after = vec![(3, INPUT_CLEAR)];
    let (lines, code, journal) = watch_journaled(&mut m, "stall-first-thawed");
    assert!(writes(&m).is_empty(), "{:?}", m.requests);
    let acts = badge_acts(&m);
    assert_eq!(acts.len(), 3, "{acts:#?}");
    assert!(acts[0].starts_with("post to=@s-9 kind=ask --wait=0 claude frozen: "));
    assert!(acts[1].starts_with("meta set attention owner=supervisor claude read: "));
    assert!(acts[2].starts_with("post to=@s-9 kind=ask --wait=0 claude read: "));
    let at = |word: &str| journal.iter().position(|l| l.starts_with(word));
    let (frozen, thawed, escalated) = (at("FROZEN"), at("THAWED"), at("ESCALATED"));
    assert!(
        frozen.is_some() && frozen < thawed && thawed < escalated,
        "{journal:#?}"
    );
    let prompts = lines
        .iter()
        .filter(|l| l.starts_with("EVENT prompt "))
        .count();
    assert_eq!(prompts, 1, "{lines:?}");
    ended_with_the_session(&lines, code);
}

/// The turn-end policy's half of the same finding: a turn that ended after
/// real work, which the host's policy continues, over a worker already
/// frozen. The `status` that named the program before typing read the
/// stall: nothing is typed, and the wait holds before its first step.
#[test]
fn a_continuation_over_a_worker_already_stalled_types_nothing() {
    let mut ended = rows(&[
        "⏺ Fixed the parser; the suite is green.",
        "",
        "✻ Worked for 3m 2s · done 4:24 PM",
        "",
    ]);
    ended.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    let mut m = Mock::new(true, vec![busy_screen(), ended]);
    m.vanish_after = Some(2);
    m.verb_replies
        .insert("post", VecDeque::from([ok("OK 7 off=91\n")]));
    m.input = INPUT_STALLED;
    let (dir, path) = journal_file("stall-first-continue");
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        policy: SupervisorConfig::default(),
        ..auto(30, None)
    };
    let (lines, code) = watch_lines_with(&mut m, &opts, |s| {
        s.set_manager(Some("@s-9".to_string()));
    });
    let journal = journal_lines(&path);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(writes(&m).is_empty(), "{:?}", m.requests);
    assert!(
        !journal.iter().any(|l| l.starts_with("CONTINUED")),
        "{journal:#?}"
    );
    let frozen = journal
        .iter()
        .find(|l| l.starts_with("FROZEN"))
        .expect("FROZEN");
    assert!(
        frozen.ends_with(" attention=none mail=OK 7 off=91"),
        "{frozen}"
    );
    assert_eq!(
        badge_acts(&m).len(),
        1,
        "the frozen mail only: {:#?}",
        m.requests
    );
    ended_with_the_session(&lines, code);
}

/// The window's host over `m`, as `harness_host.rs` builds it — its own
/// options ([`SuperviseOpts::hosted_with`]) and NO manager: the in-GUI loop
/// never calls `set_manager` — under the one limit that makes it escalate a
/// question, the owner's `answer_questions = false` (at full power the
/// question is answered, and there is no badge to withhold); journaled: the
/// printed lines, the loop's end and the journal's lines.
fn hosted_journaled(m: &mut Mock, tag: &str) -> (Vec<String>, Result<(), String>, Vec<String>) {
    let (dir, path) = journal_file(tag);
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..SuperviseOpts::hosted_with(&SupervisorConfig {
            answer_questions: false,
            ..SupervisorConfig::default()
        })
    };
    let mut out: Vec<u8> = Vec::new();
    let mut s = session(m, None);
    s.set_approval_ledger(Some(dir.join("ledger.jsonl")));
    let end = s.run_hosted(&opts, Arc::new(AtomicBool::new(false)), &mut out);
    let journal = journal_lines(&path);
    let _ = std::fs::remove_dir_all(&dir);
    let text = String::from_utf8(out).expect("utf-8");
    (text.lines().map(str::to_string).collect(), end, journal)
}

/// The worker's last words at the end of a turn that did real work, over
/// an empty composer: an idle point the host's turn-end policy escalates
/// (the worker asks for a decision), never types into.
fn idle_asking() -> Vec<String> {
    let mut r = rows(&[
        "⏺ I need your decision on the schema before I go on.",
        "",
        "✻ Worked for 3m 2s · done 4:24 PM",
        "",
    ]);
    r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    r
}

/// The requests before the first `meta set` (all of them when there is
/// none).
fn before_the_badge(m: &Mock) -> &[String] {
    let at = m
        .requests
        .iter()
        .position(|r| r.starts_with("meta set"))
        .unwrap_or(m.requests.len());
    &m.requests[..at]
}

/// Review finding on S8 (2026-09-25), the incident's own host. The window's
/// loop has no manager (`harness_host.rs` never calls `set_manager`), so a
/// point it badged with no `status` read before it — a question the worker
/// asked, an idle point the turn-end policy escalates — had no read after it
/// either (the fabric's is a manager's), and the wait probed only a host a
/// read had shown to measure the input. A worker frozen under its own
/// question kept "answer this" for the whole stall: the badge, then `await
/// seq`/`text` steps, and no `status`, no unset, no FROZEN — reproduced here
/// before the fix. Now the escalation reads `status` before its badge, and
/// a stall it reports raises none; the hold follows before the wait's first
/// step.
#[test]
fn the_windows_host_raises_no_badge_over_a_worker_frozen_under_its_point() {
    for (screen, tag) in [
        (question_screen(), "hosted-question"),
        (idle_asking(), "hosted-idle"),
    ] {
        let mut m = Mock::new(true, vec![busy_screen(), screen]);
        m.vanish_after = Some(4);
        m.input = INPUT_STALLED;
        let (lines, end, journal) = hosted_journaled(&mut m, tag);
        assert!(writes(&m).is_empty(), "{tag}: {:?}", m.requests);
        assert!(
            badge_acts(&m).is_empty(),
            "{tag}: no badge, no mail: {:#?}",
            m.requests
        );
        let escalated = journal
            .iter()
            .find(|l| l.starts_with("ESCALATED"))
            .unwrap_or_else(|| panic!("{tag}: ESCALATED: {journal:#?}"));
        assert!(
            escalated.ends_with(
                " attention=skipped: the same status says the worker is not reading its input \
                 mail=skipped: no --inbox and no $ATERM_PARENT_SESSION_ID"
            ),
            "{tag}: {escalated}"
        );
        let frozen: Vec<&String> = journal.iter().filter(|l| l.starts_with("FROZEN")).collect();
        assert_eq!(frozen.len(), 1, "{tag}: {journal:#?}");
        assert!(
            frozen[0].ends_with(
                " input=stalled wait_ms=161000 bytes=1 rss_mb=39731 attention=none \
                 mail=skipped: no --inbox and no $ATERM_PARENT_SESSION_ID"
            ),
            "{tag}: {}",
            frozen[0]
        );
        // Held before the wait's first step: the escalation's read and the
        // hold's own both come before any `await seq`.
        let first_step = m
            .requests
            .iter()
            .position(|r| r.starts_with("await seq"))
            .expect("the wait");
        assert!(
            m.requests[..first_step]
                .iter()
                .filter(|r| *r == "status")
                .count()
                >= 2,
            "{tag}: the escalation's read and the hold's: {:#?}",
            m.requests
        );
        assert!(end.is_err(), "{tag}: {end:?}");
        assert!(
            lines
                .last()
                .is_some_and(|l| l.starts_with("EXIT session gone (")),
            "{tag}: {lines:?}"
        );
    }
}

/// The same host, the worker reading its input when its question is
/// escalated and freezing under it later — the incident's own order. The
/// escalation's read says `input=clear`, so the badge goes up; that read
/// also shows the host measures the input, so the wait probes every step,
/// and the first probe that reads the stall withdraws the badge: FROZEN.
#[test]
fn the_windows_host_withdraws_the_badge_of_a_worker_that_freezes_under_it() {
    let mut m = Mock::new(true, vec![busy_screen(), question_screen()]);
    m.vanish_after = Some(4);
    // Read 1 is the escalation's, before its badge; from the wait's first
    // probe (read 2) on, the stall is read.
    m.input_after = vec![(1, INPUT_STALLED)];
    let (lines, end, journal) = hosted_journaled(&mut m, "hosted-freezes");
    assert!(writes(&m).is_empty(), "{:?}", m.requests);
    assert_eq!(
        before_the_badge(&m).last().map(String::as_str),
        Some("status"),
        "{:#?}",
        m.requests
    );
    let acts = badge_acts(&m);
    assert_eq!(acts.len(), 2, "{acts:#?}");
    assert!(
        acts[0].starts_with("meta set attention owner=supervisor claude question: "),
        "{acts:#?}"
    );
    assert_eq!(acts[1], "meta unset attention owner=supervisor");
    let frozen: Vec<&String> = journal.iter().filter(|l| l.starts_with("FROZEN")).collect();
    assert_eq!(frozen.len(), 1, "{journal:#?}");
    assert!(
        frozen[0]
            .ends_with(" attention=OK mail=skipped: no --inbox and no $ATERM_PARENT_SESSION_ID"),
        "{}",
        frozen[0]
    );
    assert!(end.is_err(), "{end:?}");
    assert!(
        lines
            .last()
            .is_some_and(|l| l.starts_with("EXIT session gone (")),
        "{lines:?}"
    );
}

/// A point that raises no badge and reads no `status` (an idle turn end
/// `watch` hands over as it is): the wait never saw a measured `input=`,
/// and before the fix it never probed, so a worker frozen there was never
/// held and the manager never told. Not knowing is no reason not to ask:
/// the first step that runs out probes, reads the stall, and holds.
#[test]
fn a_wait_that_never_read_status_probes_and_holds() {
    let mut m = Mock::new(true, vec![busy_screen(), idle_screen()]);
    m.vanish_after = Some(3);
    m.verb_replies
        .insert("post", VecDeque::from([ok("OK 7 off=91\n")]));
    m.input = INPUT_STALLED;
    let (lines, code, journal) = watch_journaled(&mut m, "stall-unread-idle");
    assert!(writes(&m).is_empty(), "{:?}", m.requests);
    assert_eq!(
        badge_acts(&m),
        [
            "post to=@s-9 kind=ask --wait=0 claude frozen: not reading input for 2m41s (1 B \
             queued, rss 38.8 GB); pressing nothing until it reads again \u{2014} restart it: \
             signal term (signal kill if it survives), then claude --continue"
        ],
        "{:#?}",
        m.requests
    );
    let first_status = m
        .requests
        .iter()
        .position(|r| r == "status")
        .expect("the probe");
    assert!(
        m.requests[first_status - 2].starts_with("await seq"),
        "the first step's probe: {:#?}",
        m.requests
    );
    let frozen = journal
        .iter()
        .find(|l| l.starts_with("FROZEN"))
        .expect("FROZEN");
    assert!(
        frozen.ends_with(" attention=none mail=OK 7 off=91"),
        "{frozen}"
    );
    ended_with_the_session(&lines, code);
}

/// NEGATIVE CONTROL for both: the window's host over a host that publishes
/// no `input=`, or `input=-`. The escalation's read learns so — the one
/// `status` more this host sees — the badge goes up as it always did, and
/// no step is probed after it: no hold, no FROZEN.
#[test]
fn the_windows_host_asks_a_host_that_does_not_measure_input_once() {
    for input in ["", "input=- input_bytes=- input_wait_ms=- fg_rss_mb=-"] {
        let mut m = Mock::new(true, vec![busy_screen(), question_screen()]);
        m.vanish_after = Some(4);
        m.input = input;
        let (lines, end, journal) = hosted_journaled(&mut m, "hosted-none");
        assert_eq!(count(&m, "status"), 1, "input={input:?}: {:#?}", m.requests);
        assert_eq!(
            before_the_badge(&m).last().map(String::as_str),
            Some("status"),
            "input={input:?}: {:#?}",
            m.requests
        );
        let acts = badge_acts(&m);
        assert_eq!(acts.len(), 1, "input={input:?}: {acts:#?}");
        assert!(acts[0].starts_with("meta set attention owner=supervisor claude question: "));
        assert!(
            !journal
                .iter()
                .any(|l| l.starts_with("FROZEN") || l.starts_with("THAWED")),
            "input={input:?}: {journal:#?}"
        );
        assert!(end.is_err(), "input={input:?}: {end:?}");
        assert!(
            lines
                .last()
                .is_some_and(|l| l.starts_with("EXIT session gone (")),
            "input={input:?}: {lines:?}"
        );
    }
}
