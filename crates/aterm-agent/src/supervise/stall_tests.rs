// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The stall hold (`stall.rs`) over the scripted server. Included from `run_tests.rs`
// (`mod stall_hold`), so its `Mock` and helpers are in scope.

use crate::harness::resume::AfterRestart;

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

/// A worker showing a box no rule approves (a Read of `~/.ssh/config`) that never leaves the
/// screen, three post replies in hand.
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

/// The end each of these runs must reach: the session gone under the wait, never a refusal read
/// as the loop's own fault.
fn ended_with_the_session(lines: &[String], code: u8) {
    let last = lines.last().expect("an end line");
    assert!(
        last.starts_with("EXIT session gone (") && last.ends_with("ERR exited)"),
        "{lines:?}"
    );
    assert_eq!(code, 1);
}

/// A worker that stops reading under an escalated box: the first probe that reads
/// `input=stalled` unsets the badge, mails the manager one `kind=ask` and journals `FROZEN …`;
/// nothing is pressed, typed or escalated while it stands.
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
         signal term (signal kill if it survives)"
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

/// The stall lifts (`input=clear`) while the box is still on screen: `THAWED …` is journaled and
/// the box decided afresh, escalated again with badge and ask. Nothing is pressed meanwhile.
#[test]
fn a_thawed_worker_has_its_point_decided_again() {
    let mut m = box_that_sits();
    // Reading its input through the box's escalation (reads 1 and 2), stalled
    // from the wait's first probe (read 3), clear again from its third (read
    // 5), and the next look thaws.
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

/// The loop tells its host the stall it holds and that it lifted
/// ([`IdleHost::stalled`]), a notice of what it saw. Negative control: a
/// worker that never stalls tells the host nothing.
#[test]
fn the_host_is_told_the_stall_it_holds_and_that_it_lifted() {
    #[derive(Debug, Default)]
    struct Told(std::sync::Mutex<Vec<bool>>);
    impl IdleHost for Told {
        fn wants(&self) -> bool {
            false
        }
        fn at_idle(&self) -> Option<HostStep> {
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

/// The frozen mail's remedy: the agent's resume line where the host would not carry the explicit
/// restart; the restart and the relaunch where it would ([`IdleHost::restart_offered`]), whatever
/// `[harness] relaunch` says; the restart alone where the conversation is not promised. Never
/// `claude --continue`.
#[test]
fn the_frozen_mail_names_the_workers_own_conversation_or_the_hosts_relaunch() {
    #[derive(Debug)]
    struct Host {
        restarts: bool,
        offered: Option<bool>,
    }
    impl IdleHost for Host {
        fn wants(&self) -> bool {
            false
        }
        fn at_idle(&self) -> Option<HostStep> {
            None
        }
        fn owns_turn_end(&self) -> bool {
            false
        }
        fn can_restart(&self) -> bool {
            self.restarts
        }
        fn restart_offered(&self) -> Option<bool> {
            self.offered
        }
        fn resume_command(&self) -> Option<String> {
            Some("claude --model opus --resume 5f1c2d3e-4b5a-4c6d-8e7f-0a1b2c3d4e5f".to_string())
        }
    }
    let command = "signal term (signal kill if it survives), then claude --model opus --resume \
                   5f1c2d3e-4b5a-4c6d-8e7f-0a1b2c3d4e5f";
    let carried = "restart if-stalled; aterm relaunches it on its conversation";
    for (restarts, offered, tail) in [
        (false, None, command),
        // The host carries the act: the explicit restart, which ends a
        // survivor itself.
        (true, Some(true), carried),
        // ...under `[harness] relaunch = false` too.
        (false, Some(true), carried),
        // Restarts Claude Code, but its relaunch would refuse this launch.
        (true, None, command),
        // Carried, its conversation not promised: the restart alone.
        (false, Some(false), "restart it: restart if-stalled"),
    ] {
        let relaunches = (restarts, offered);
        let mut m = box_that_sits();
        m.input_after = vec![(2, INPUT_STALLED)];
        let host = Host { restarts, offered };
        let opts = SuperviseOpts {
            idle_host: Some(Arc::new(host) as Arc<dyn IdleHost>),
            ..auto(30, None)
        };
        let _ = watch_lines_with(&mut m, &opts, |s| {
            s.set_manager(Some("@s-9".to_string()));
        });
        let acts = badge_acts(&m);
        let mail = acts
            .iter()
            .find(|a| a.contains(" claude frozen: "))
            .unwrap_or_else(|| panic!("the frozen mail: {acts:#?}"));
        assert!(
            mail.ends_with(tail),
            "(restarts, offered)={relaunches:?}: {mail}"
        );
        assert!(!mail.contains("--continue"), "{mail}");
    }
}

/// A press refused `ERR busy input-unread …` (the gate fires at 1 s, the stall is published at
/// 10 s): the loop backs off as for any `ERR busy`, and its next look reads `status` before it
/// presses again, which here holds: one press, never a second.
#[test]
fn a_press_refused_input_unread_reads_status_before_it_presses_again() {
    let mut m = Mock::new(true, vec![busy_screen(), bash_one_row()]);
    m.vanish_after = Some(4);
    m.key_replies.push_back(err(
        "busy input-unread bytes=1 wait_ms=1500 input=pending (the program has not read \
         input queued 1s ago; retry in a moment)",
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

/// A press written behind unread input (`OK seq=<n> warn=input-unread …`): the next look reads
/// `status` before pressing again, as after a refusal, and the stall published by then holds it.
#[test]
fn a_press_warned_input_unread_reads_status_before_it_presses_again() {
    let mut m = Mock::new(true, vec![busy_screen(), bash_one_row()]);
    m.vanish_after = Some(4);
    m.key_replies.push_back(ok(
        "OK seq=101 warn=input-unread input_bytes=1 input_wait_ms=1500\n",
    ));
    m.input_after = vec![(1, INPUT_STALLED)];
    let (lines, code, journal) = watch_journaled(&mut m, "stall-warned");
    let presses = writes(&m);
    assert_eq!(presses.len(), 1, "{:?}", m.requests);
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
    assert!(
        !journal.iter().any(|l| l.starts_with("REFUSED")),
        "a warned press was written, not refused: {journal:#?}"
    );
    ended_with_the_session(&lines, code);
}

/// A reply exactly as a transport hands it to the loop ([`crate::supervise::transport::render`]:
/// a `turn`'s verdict on stderr, its settled rows on stdout, as `aterm-ctl` prints them).
fn rendered(verb: &str, header: &str, body: &str) -> CtlReply {
    let framing = aterm_types::control_verbs::framing_of(verb, verb);
    crate::supervise::transport::render(verb, framing, header, body.as_bytes())
}

/// A `turn` the gate warned, as the server answers it: the warning at the
/// end of the verdict line (`input_stall::with_warning`), then the rows.
const WARNED_TURN: &str = "OK 1 turn submitted=1 status=settled seq=9 id=2 dur_ms=5 hash=00 \
    warn=input-unread input_bytes=2 input_wait_ms=1100";

/// `warned_input_unread` reads the gate's warning on the reply's first line as each transport
/// hands it on (a `key`'s `OK` on stdout, a `turn`'s verdict on stderr), and nowhere else.
#[test]
fn a_warning_is_read_off_the_first_line_the_server_wrote() {
    let key = rendered(
        "key",
        "OK seq=5 warn=input-unread input_bytes=1 input_wait_ms=1200",
        "",
    );
    assert!(key.warned_input_unread(), "{key:?}");
    let turn = rendered("turn", WARNED_TURN, "row\n");
    // The shape the transports produce: the verdict (and its warning) on
    // stderr, stdout the rows alone.
    assert_eq!(turn.stdout, "row\n", "{turn:?}");
    assert!(turn.stderr.contains("warn=input-unread"), "{turn:?}");
    assert!(turn.warned_input_unread(), "{turn:?}");
    assert!(turn.submitted(), "{turn:?}");
    for reply in [
        rendered("key", "OK seq=5", ""),
        rendered("key", "OK seq=5 nowarn=input-unread", ""),
        rendered(
            "turn",
            "OK 1 turn submitted=1 status=settled seq=9 id=2 dur_ms=5 hash=00",
            "warn=input-unread\n",
        ),
        rendered(
            "turn",
            "OK 2 turn submitted=1 status=settled seq=9 id=2 dur_ms=5 hash=00",
            "row\nwarn=input-unread\n",
        ),
        err("busy input-unread bytes=1 wait_ms=12000 input=stalled (…)"),
    ] {
        assert!(!reply.warned_input_unread(), "{reply:?}");
    }
}

/// A warned continuation: the `status` before typing saw the worker reading, so the `turn` is
/// typed, and the server wrote it behind unread input (`warn=` on the verdict). The wait after it
/// reads `status` first and holds the stall published by then. Negative control: after an
/// unwarned `turn` no `status` is read, so that stall is not seen.
#[test]
fn a_continuation_warned_input_unread_reads_status_at_the_next_look() {
    let run = |warned: bool| {
        let mut ended = rows(&[
            "⏺ Fixed the parser; the suite is green.",
            "",
            "✻ Worked for 3m 2s · done 4:24 PM",
            "",
        ]);
        ended.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        let mut m = Mock::new(true, vec![busy_screen(), ended, busy_screen()]);
        m.vanish_after = Some(3);
        m.verb_replies
            .insert("post", VecDeque::from([ok("OK 7 off=91\n")]));
        let header = if warned {
            WARNED_TURN
        } else {
            "OK 1 turn submitted=1 status=settled seq=9 id=2 dur_ms=5 hash=00"
        };
        m.verb_replies
            .insert("turn", VecDeque::from([rendered("turn", header, "row\n")]));
        m.input_after = vec![(1, INPUT_STALLED)];
        let (dir, path) = journal_file(if warned {
            "stall-turn-warned"
        } else {
            "stall-turn-plain"
        });
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
        (m, lines, code, journal)
    };
    // The requests after the one `turn`, of a run.
    let after_turn = |m: &Mock| -> Vec<String> {
        let at = m
            .requests
            .iter()
            .position(|r| r.starts_with("turn"))
            .expect("the turn");
        m.requests[at + 1..].to_vec()
    };
    let (m, lines, code, journal) = run(true);
    assert_eq!(writes(&m).len(), 1, "one turn: {:#?}", m.requests);
    let after = after_turn(&m);
    assert_eq!(
        after.first().map(String::as_str),
        Some("status"),
        "the warned turn's wait reads status first: {after:#?}"
    );
    assert!(
        journal.iter().any(|l| l.starts_with("FROZEN")),
        "{journal:#?}"
    );
    ended_with_the_session(&lines, code);

    let (m, lines, code, journal) = run(false);
    assert_eq!(writes(&m).len(), 1, "one turn: {:#?}", m.requests);
    let after = after_turn(&m);
    assert!(
        !after.iter().any(|r| r == "status"),
        "an unwarned turn reads no status after it: {after:#?}"
    );
    assert!(
        !journal.iter().any(|l| l.starts_with("FROZEN")),
        "{journal:#?}"
    );
    ended_with_the_session(&lines, code);
}

/// Negative control: a host that publishes no `input=`, or `input=-` (not macOS, not a tty), gets
/// no probe and no hold. The `text` after each step is the wait's own (it sees a file box replaced
/// under its path row); the `meta` before the badge reads the agent's directory.
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
                "await gone esc.to.interrupt timeout 3000",
                "text --json tail=40",
                "await gone esc.to.interrupt rows 2 7 timeout 20000",
                "text --json tail=40",
                "status",
                "meta",
                set.as_str(),
                "status",
                post.as_str(),
                anchor,
                "text --json tail=40",
                anchor,
                "text --json tail=40",
                anchor,
                "text --json tail=40",
                anchor,
                "text --json tail=40",
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

/// A loop that starts over a worker already frozen reads `input=stalled` in the `status` that
/// named the box's program and holds before anything is pressed or badged, for a box a rule
/// approves (a `git log`) and one none does.
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
              signal term (signal kill if it survives)"
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

/// The same for a point with no `status` before its escalation (a question): the badge goes up
/// before the escalation's own read reports the stall, but that read is acted on: no ask is mailed
/// and the hold withdraws the badge before the wait's first step. One mail, the frozen one.
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

/// Stalled from the loop's first read, reading again later: the box sits through the stall
/// undecided and is decided once, after `THAWED`.
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

/// A continuation over a worker already frozen: the `status` read before typing reports the stall,
/// so nothing is typed and the wait holds before its first step.
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

/// The window's host over `m` ([`SuperviseOpts::hosted_with`], no manager) with
/// `answer_questions = false`, the one limit that makes it escalate a question: the printed lines,
/// the loop's end and the journal's lines.
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

/// A turn end after real work over an empty composer, asking for a decision: an idle point the
/// turn-end policy escalates, never types into.
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

/// The requests before the first `meta set` (all of them if there is none).
fn before_the_badge(m: &Mock) -> &[String] {
    let at = m
        .requests
        .iter()
        .position(|r| r.starts_with("meta set"))
        .unwrap_or(m.requests.len());
    &m.requests[..at]
}

/// The window's host has no manager, so a point it badges (a question, an idle point the turn-end
/// policy escalates) reads `status` itself before the badge, and a stall that read reports raises
/// none; the hold follows before the wait's first step.
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

/// The same host, the worker freezing after its question is escalated: the escalation's read says
/// `input=clear`, so the badge goes up and every wait step probes; the first probe reading the
/// stall withdraws the badge.
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

/// A point that raises no badge and reads no `status` (an idle turn end `watch` hands over): not
/// knowing whether input is measured, the first step that runs out probes, reads the stall, holds.
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
             signal term (signal kill if it survives)"
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

/// Negative control for both: the window's host over a server that publishes no `input=`, or
/// `input=-`, sees one `status` (the escalation's, which learns so), the badge, and no probe.
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

/// The explicit restart the remedy sends under `stall_term = true` for the
/// host's agent (pid 4242), within the default grace.
const RESTART: &str = "restart pid=4242 if-stalled quiet=120";

/// A worker frozen past the remedy's bound: eleven minutes.
const INPUT_STALLED_LONG: &str = "input=stalled input_bytes=1 input_wait_ms=660000 fg_rss_mb=39731";

/// A host that relaunches an agent the stall's remedy ends, or not (field 0), keeps what it was
/// told for a person (field 1), and offers field 2 as what follows the end
/// ([`IdleHost::restart_offered`], [`IdleHost::resume_command`]).
#[derive(Debug)]
struct Relaunching(bool, Mutex<Vec<String>>, AfterRestart);

impl IdleHost for Relaunching {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn relaunches_after_stall(&self) -> bool {
        self.0
    }
    fn agent_pid(&self) -> Option<u32> {
        Some(4242)
    }
    fn restart_offered(&self) -> Option<bool> {
        match self.2 {
            AfterRestart::Relaunch => Some(true),
            AfterRestart::Act => Some(false),
            AfterRestart::Unknown | AfterRestart::Command(_) => None,
        }
    }
    fn resume_command(&self) -> Option<String> {
        match &self.2 {
            AfterRestart::Command(line) => Some(line.clone()),
            _ => None,
        }
    }
    fn inform(&self, text: &str) {
        self.1
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(text.to_string());
    }
}

/// A frozen worker under a relaunching host with `stall_term = true` (the default warns:
/// [`warn_run`]): `input` from the wait's first probe on, `term_after_s` the bound, `tweak` the
/// rest. Returns the signal and restart requests and the lines said.
fn remedy_run(
    input: &'static str,
    relaunches: bool,
    term_after_s: u32,
    tweak: impl FnOnce(&mut Mock),
) -> (Vec<String>, Vec<String>) {
    let (signals, lines, _) = remedy_run_as(
        input,
        relaunches,
        term_after_s,
        true,
        AfterRestart::Unknown,
        tweak,
    );
    (signals, lines)
}

/// [`remedy_run`] under the default policy (`stall_term = false`): the signal requests, the lines
/// said, and what the host was told for a person.
fn warn_run(
    input: &'static str,
    relaunches: bool,
    term_after_s: u32,
    tweak: impl FnOnce(&mut Mock),
) -> (Vec<String>, Vec<String>, Vec<String>) {
    remedy_run_as(
        input,
        relaunches,
        term_after_s,
        false,
        AfterRestart::Relaunch,
        tweak,
    )
}

fn remedy_run_as(
    input: &'static str,
    relaunches: bool,
    term_after_s: u32,
    stall_term: bool,
    after: AfterRestart,
    tweak: impl FnOnce(&mut Mock),
) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut m = box_that_sits();
    m.input_after = vec![(2, input)];
    tweak(&mut m);
    let host = Arc::new(Relaunching(relaunches, Mutex::new(Vec::new()), after));
    let mut opts = SuperviseOpts {
        idle_host: Some(host.clone() as Arc<dyn IdleHost>),
        ..auto(30, None)
    };
    opts.policy.stall_term_after_s = term_after_s;
    opts.policy.stall_term = stall_term;
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_manager(Some("@s-9".to_string()));
    });
    let signals = m
        .requests
        .iter()
        .filter(|r| r.starts_with("signal") || r.starts_with("restart"))
        .cloned()
        .collect();
    let informed = host
        .1
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    (signals, lines, informed)
}

/// The default is a warning, not an end: past the bound under a relaunching host, with no hand on
/// it, no signal but one `STALL-WARNED … ended=no` line and one record for the person. Negative
/// controls: the record's second line where the host would not carry the act or promise the
/// conversation, and silence where the strict remedy would not act.
#[test]
fn a_worker_frozen_past_the_bound_is_warned_about_and_left_running() {
    assert!(!SupervisorConfig::default().stall_term, "the default warns");
    let default = SupervisorConfig::default().stall_term_after_s;
    let (signals, lines, informed) = warn_run(INPUT_STALLED_LONG, true, default, |_| {});
    assert!(
        signals.is_empty(),
        "no signal by default: {signals:?} {lines:#?}"
    );
    let warned: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("STALL-WARNED"))
        .collect();
    assert_eq!(warned.len(), 1, "once an episode: {lines:#?}");
    assert!(
        warned[0].contains(" input=stalled wait_ms=660000 rss_mb=39731 ended=no ")
            && warned[0].contains("stall_term = true"),
        "{}",
        warned[0]
    );
    assert!(
        !lines.iter().any(|l| l.starts_with("SIGNALLED")),
        "{lines:#?}"
    );
    assert_eq!(informed.len(), 1, "one record for the person: {informed:?}");
    // The first line is the state (the record's title); the second, that it is left running, the
    // explicit restart that ends it, and that the host brings the conversation back.
    let mut said = informed[0].lines();
    assert_eq!(
        said.next(),
        Some("claude frozen: not reading input for 11m00s (1 B queued, rss 38.8 GB)"),
        "{}",
        informed[0]
    );
    let act = said.next().unwrap_or_default();
    assert!(
        act.starts_with("aterm leaves it running")
            && act.contains(" restart pid=4242 if-stalled` ends it")
            && !act.contains("signal")
            && act.ends_with(&format!(", and {}", crate::harness::resume::RELAUNCH_WORDS)),
        "{}",
        informed[0]
    );
    assert!(said.next().is_none(), "{}", informed[0]);
    assert!(
        !informed[0].contains("stall_term"),
        "one action: {}",
        informed[0]
    );
    // Negative controls: no relaunch promised for this launch. The resume line named instead is
    // whole: one too long for the detail line gives its bare form, never a cut id.
    let line = "claude --resume 5f1c2d3e-4b5a-4c6d-8e7f-0a1b2c3d4e5f";
    let long = format!(
        "claude --add-dir /{} --resume {}",
        "d".repeat(160),
        &line[16..]
    );
    for (after, tail) in [
        (
            AfterRestart::Command(line.to_string()),
            format!("(`signal kill` if it survives), then `{line}` resumes it"),
        ),
        (
            AfterRestart::Command(long),
            format!("(`signal kill` if it survives), then `{line}` resumes it"),
        ),
        (
            AfterRestart::Unknown,
            "(`signal kill` if it survives)".to_string(),
        ),
        (
            AfterRestart::Act,
            " restart pid=4242 if-stalled` restarts it".to_string(),
        ),
    ] {
        let (_, _, informed) =
            remedy_run_as(INPUT_STALLED_LONG, true, default, false, after, |_| {});
        assert_eq!(informed.len(), 1, "{informed:?}");
        let act = informed[0].lines().nth(1).unwrap_or_default();
        assert!(
            act.ends_with(&tail)
                && !act.contains(crate::harness::resume::RELAUNCH_WORDS)
                && act.chars().count() <= 240,
            "{}",
            informed[0]
        );
    }
    for (why, input, bound, person) in [
        ("younger than the bound", INPUT_STALLED, default, false),
        ("the bound switched off", INPUT_STALLED_LONG, 0, false),
        (
            "a person typed within the grace",
            INPUT_STALLED_LONG,
            default,
            true,
        ),
    ] {
        let (signals, lines, informed) = warn_run(input, true, bound, |m| {
            if person {
                m.human_ms = Some(1_000);
            }
        });
        assert!(signals.is_empty(), "{why}: {signals:?}");
        assert!(
            informed.is_empty() && !lines.iter().any(|l| l.starts_with("STALL-WARNED")),
            "{why}: {informed:?} {lines:#?}"
        );
    }
}

/// With `stall_term`, a worker frozen past `[harness] stall_term_after_s` under a relaunching host
/// with no hand on it gets `restart pid=<agent> if-stalled quiet=<grace>` (said `RESTARTED …`)
/// once, and never a `signal kill` of the loop's.
#[test]
fn a_worker_frozen_past_the_bound_is_ended_for_its_relaunch() {
    let default = SupervisorConfig::default().stall_term_after_s;
    assert_eq!(default, 600, "ten minutes");
    let (signals, lines) = remedy_run(INPUT_STALLED_LONG, true, default, |_| {});
    assert_eq!(signals, [RESTART], "never a kill: {lines:#?}");
    let said: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("RESTARTED"))
        .collect();
    assert_eq!(said.len(), 1, "{lines:#?}");
    assert!(
        said[0].contains(
            " input=stalled wait_ms=660000 rss_mb=39731 reply=OK restarting pid 4242 cause=stall"
        ),
        "{}",
        said[0]
    );

    // Negative controls.
    for (why, input, relaunches, bound, tweak) in [
        ("younger than the bound", INPUT_STALLED, true, default, None),
        (
            "a bound written longer",
            INPUT_STALLED_LONG,
            true,
            3600,
            None,
        ),
        ("the remedy switched off", INPUT_STALLED_LONG, true, 0, None),
        (
            "no host relaunches it",
            INPUT_STALLED_LONG,
            false,
            default,
            None,
        ),
        (
            "a person typed within the grace",
            INPUT_STALLED_LONG,
            true,
            default,
            Some("person"),
        ),
        (
            "a lease holds the session",
            INPUT_STALLED_LONG,
            true,
            default,
            Some("lease"),
        ),
        (
            "a stopped job's remedy is `signal cont`, a person's",
            "input=stopped input_bytes=1 input_wait_ms=660000 fg_rss_mb=-",
            true,
            default,
            None,
        ),
    ] {
        let (signals, lines) = remedy_run(input, relaunches, bound, |m| match tweak {
            Some("person") => m.human_ms = Some(1_000),
            Some("lease") => m.hand = "lease:@s-7",
            _ => {}
        });
        assert!(signals.is_empty(), "{why}: {signals:?} {lines:#?}");
    }
}

/// A stopped job's `status`, its input waiting `wait_ms`.
fn input_stopped(aged: bool) -> &'static str {
    if aged {
        "input=stopped input_bytes=1 input_wait_ms=660000 fg_rss_mb=-"
    } else {
        "input=stopped input_bytes=1 input_wait_ms=161000 fg_rss_mb=-"
    }
}

/// Under `stall_term = true` the remedy is the server's `restart … if-stalled`: `ERR not-stalled`
/// is not counted (the next stalled read retries); `ERR restart unavailable` is counted and the
/// warning said instead; a server without the verb gets the bare `signal term`.
#[test]
fn the_remedy_is_the_servers_restart_and_its_refusals_decide() {
    let default = SupervisorConfig::default().stall_term_after_s;
    let (signals, lines) = remedy_run(INPUT_STALLED_LONG, true, default, |m| {
        m.restart_replies.push_back(err("not-stalled"));
    });
    assert_eq!(signals, [RESTART, RESTART], "{lines:#?}");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("RESTART-REFUSED") && l.ends_with("reply=ERR not-stalled")),
        "{lines:#?}"
    );
    let (signals, lines, informed) = remedy_run_as(
        INPUT_STALLED_LONG,
        true,
        default,
        true,
        AfterRestart::Relaunch,
        |m| {
            m.restart_replies
                .push_back(err("restart unavailable: not-supervised"))
        },
    );
    assert_eq!(signals, [RESTART], "counted: once an episode: {lines:#?}");
    let warned: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("STALL-WARNED"))
        .collect();
    assert_eq!(warned.len(), 1, "{lines:#?}");
    assert!(
        warned[0].ends_with("ended=no (ERR restart unavailable: not-supervised)"),
        "{}",
        warned[0]
    );
    assert_eq!(informed.len(), 1, "{informed:?}");
    assert!(
        informed[0].contains("signal term") && !informed[0].contains("restart pid="),
        "the warning names what a person can do: {}",
        informed[0]
    );
    // A server without the verb: `ERR unknown verb`, or `ERR denied` for a verb aimed at a session.
    for older in ["unknown verb (try: help)", "denied"] {
        let (signals, lines) = remedy_run(INPUT_STALLED_LONG, true, default, |m| {
            m.restart_replies.push_back(err(older));
        });
        assert_eq!(signals, [RESTART, "signal term"], "{older}: {lines:#?}");
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("SIGNALLED") && l.contains(" signal=term ")),
            "{older}: {lines:#?}"
        );
    }
}

/// Tier-1 of `SupervisorStallRemedy` (aterm-spec `supervisor_stall_remedy_model`): in every
/// configuration the model reaches with no term sent, the real loop sends the explicit restart
/// exactly where `Term` is enabled, once per episode (`Thaw` starts a new one), and the default
/// warning takes the same gate. Negative control: where the `Buggy = 1` model acts under a hand
/// or kills after the act, the loop sends nothing, or one restart (a survivor's SIGKILL is the
/// server's).
#[test]
fn tier1_the_real_remedy_signals_exactly_where_the_model_terms() {
    use std::collections::{BTreeMap, BTreeSet, VecDeque};
    let m = aterm_spec::derive::supervisor_stall_remedy_model();
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([m.init_state()]);
    // (stalled, aged, off, hand, host) → the model terms there.
    let mut configs = BTreeMap::new();
    while let Some(st) = queue.pop_front() {
        if !seen.insert(st.clone()) {
            continue;
        }
        if st["stalled"] > 0 && st["termed"] == 0 {
            let key = (st["stalled"], st["aged"], st["off"], st["hand"], st["host"]);
            configs.insert(key, m.action_enabled("Term", &st));
            if m.action_enabled("Term", &st) {
                let mut after = st.clone();
                assert!(m.fire("Term", &mut after));
                assert!(!m.action_enabled("Term", &after), "once: {after:?}");
            }
        }
        for action in &m.actions {
            let mut next = st.clone();
            if m.fire(action.name, &mut next) {
                queue.push_back(next);
            }
        }
    }
    assert_eq!(configs.len(), 32, "every configuration is reachable");
    assert_eq!(
        configs.values().filter(|t| **t).count(),
        1,
        "one configuration terms: {configs:?}"
    );
    for (&(stalled, aged, off, hand, host), &term) in &configs {
        let input = match (stalled, aged) {
            (1, 1) => INPUT_STALLED_LONG,
            (1, _) => INPUT_STALLED,
            (_, a) => input_stopped(a == 1),
        };
        let bound = if off == 1 { 0 } else { 600 };
        let (signals, lines) = remedy_run(input, host == 1, bound, |mk| {
            if hand == 1 {
                mk.human_ms = Some(1_000);
            }
        });
        let want: &[&str] = if term { &[RESTART] } else { &[] };
        assert_eq!(
            signals, want,
            "stalled={stalled} aged={aged} off={off} hand={hand} host={host}: {lines:#?}"
        );
        // The default takes the same gate: never a signal, one warning exactly where `Term` is.
        let (signals, lines, informed) = warn_run(input, host == 1, bound, |mk| {
            if hand == 1 {
                mk.human_ms = Some(1_000);
            }
        });
        let warned = lines
            .iter()
            .filter(|l| l.starts_with("STALL-WARNED"))
            .count();
        assert!(signals.is_empty(), "the default never signals: {signals:?}");
        assert_eq!(
            (warned, informed.len()),
            if term { (1, 1) } else { (0, 0) },
            "stalled={stalled} aged={aged} off={off} hand={hand} host={host}: {lines:#?}"
        );
    }
    // A new episode (thawed, frozen again) gets its own term: the model's `Thaw` forgets it.
    let mut st = m.init_state();
    for action in ["Freeze", "Age", "Term", "Thaw", "Freeze", "Age"] {
        assert!(m.fire(action, &mut st), "{action} at {st:?}");
    }
    assert!(m.action_enabled("Term", &st));
    let (signals, lines) = remedy_run(INPUT_STALLED_LONG, true, 600, |mk| {
        mk.input_after = vec![
            (2, INPUT_STALLED_LONG),
            (4, INPUT_CLEAR),
            (6, INPUT_STALLED_LONG),
        ];
        mk.vanish_after = Some(8);
    });
    assert_eq!(signals, [RESTART, RESTART], "{lines:#?}");

    // Negative controls: where the Buggy model signals, the real loop does not.
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    let mut st = buggy.init_state();
    for action in ["HandOn", "Freeze", "Age"] {
        assert!(buggy.fire(action, &mut st), "{action} at {st:?}");
    }
    assert!(buggy.action_enabled("TermBlind", &st), "blind to the hand");
    let (signals, _) = remedy_run(INPUT_STALLED_LONG, true, 600, |mk| {
        mk.human_ms = Some(1_000);
    });
    assert!(signals.is_empty(), "{signals:?}");
    let mut st = buggy.init_state();
    for action in ["Freeze", "Age", "Term"] {
        assert!(buggy.fire(action, &mut st), "{action} at {st:?}");
    }
    assert!(buggy.action_enabled("Kill", &st) && buggy.action_enabled("TermAgain", &st));
    let (signals, _) = remedy_run(INPUT_STALLED_LONG, true, 600, |_| {});
    assert_eq!(signals, [RESTART], "one restart, no kill of the loop's");
}

/// BESIDE A PROVEN SELF-CLAIM THE STALL'S REMEDY STANDS (the codex-alab
/// self-claim contract, §4): a loop the host runs beside a proven self-claim
/// ([`Session::set_beside`]) takes no claim, types and badges nothing — yet
/// a worker frozen past the bound still gets aterm's explicit restart, which
/// the agent cannot give itself. NEGATIVE CONTROL: behind another
/// supervisor's claim (`ERR busy supervisor=<holder>`) the same freeze gets
/// none — that supervisor answers for the session.
#[test]
fn beside_a_proven_self_claim_the_stalls_remedy_stands_and_nothing_is_typed() {
    let default = SupervisorConfig::default().stall_term_after_s;
    let run = |beside: bool| {
        let mut m = box_that_sits();
        m.input_after = vec![(2, INPUT_STALLED_LONG)];
        if !beside {
            m.claim_replies = [err("busy supervisor=watcher%40elsewhere")].into();
        }
        let host = Arc::new(Relaunching(
            true,
            Mutex::new(Vec::new()),
            AfterRestart::Unknown,
        ));
        let mut opts = SuperviseOpts {
            idle_host: Some(host as Arc<dyn IdleHost>),
            ..auto(30, None)
        };
        opts.policy.stall_term_after_s = default;
        opts.policy.stall_term = true;
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            if beside {
                s.set_beside(Some("codex-alab@4242".to_string()));
            }
        });
        let remedies: Vec<String> = m
            .requests
            .iter()
            .filter(|r| r.starts_with("signal") || r.starts_with("restart"))
            .cloned()
            .collect();
        let claims: Vec<String> = m
            .requests
            .iter()
            .filter(|r| r.starts_with("meta set supervisor"))
            .cloned()
            .collect();
        let typed = writes(&m).len();
        (remedies, claims, typed, badge_acts(&m), lines)
    };
    let (remedies, claims, typed, badges, lines) = run(true);
    assert_eq!(remedies, [RESTART], "the remedy stands beside: {lines:#?}");
    assert!(claims.is_empty(), "nothing claimed beside: {claims:?}");
    assert_eq!(typed, 0, "nothing typed: {lines:#?}");
    assert!(
        badges.iter().all(|b| !b.contains("attention")),
        "nothing badged: {badges:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("WATCHING a proven self-claim supervises this session")),
        "{lines:#?}"
    );
    let (remedies, _, typed, _, lines) = run(false);
    assert!(remedies.is_empty(), "behind another holder: {lines:#?}");
    assert_eq!(typed, 0, "{lines:#?}");
}
