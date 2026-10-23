// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::harness::resume::AfterRestart;

fn writes(m: &Mock) -> Vec<&String> {
    m.requests
        .iter()
        .filter(|r| r.starts_with("key") || r.starts_with("send") || r.starts_with("turn"))
        .collect()
}

fn badge_acts(m: &Mock) -> Vec<String> {
    m.requests
        .iter()
        .filter(|r| r.starts_with("meta set") || r.starts_with("meta unset"))
        .cloned()
        .collect()
}

fn journal_lines(path: &Path) -> Vec<String> {
    journal_records(path)
        .0
        .into_iter()
        .map(|r| r.line)
        .collect()
}

fn watch_journaled(m: &mut Mock, tag: &str) -> (Vec<String>, u8, Vec<String>) {
    let (dir, path) = journal_file(tag);
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..auto(30)
    };
    let (lines, code) = watch_lines(m, &opts);
    let journal = journal_lines(&path);
    let _ = std::fs::remove_dir_all(&dir);
    (lines, code, journal)
}

fn box_that_sits() -> Mock {
    let mut m = Mock::new(vec![busy_screen(), read_box()]);
    m.vanish_after = Some(4);
    m
}

fn ended_with_the_session(lines: &[String], code: u8) {
    let last = lines.last().expect("an end line");
    assert!(
        last.starts_with("EXIT session gone (") && last.ends_with("ERR exited)"),
        "{lines:?}"
    );
    assert_eq!(code, 1);
}

#[test]
fn a_frozen_worker_is_held_and_its_badge_withdrawn() {
    let mut m = box_that_sits();
    m.input_after = vec![(2, INPUT_STALLED)];
    let (lines, code, journal) = watch_journaled(&mut m, "stall-held");
    assert!(writes(&m).is_empty(), "{:?}", m.requests);
    let acts = badge_acts(&m);
    assert_eq!(acts.len(), 2, "{acts:#?}");
    assert!(
        acts[0].starts_with("meta set attention owner=supervisor claude read: ~/.ssh/config ("),
        "{acts:#?}"
    );
    assert_eq!(acts[1], "meta unset attention owner=supervisor");
    let probes = m.requests.iter().filter(|r| *r == "status").count();
    assert!(probes >= 5, "a probe per step: {:?}", m.requests);
    let frozen: Vec<&String> = journal.iter().filter(|l| l.starts_with("FROZEN")).collect();
    assert_eq!(frozen.len(), 1, "{journal:#?}");
    assert!(
        frozen[0].ends_with(" input=stalled wait_ms=161000 bytes=1 rss_mb=39731 attention=OK"),
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

#[test]
fn a_thawed_worker_has_its_point_decided_again() {
    let mut m = box_that_sits();
    m.input_after = vec![(2, INPUT_STALLED), (4, INPUT_CLEAR)];
    let (lines, code, journal) = watch_journaled(&mut m, "stall-thawed");
    assert!(writes(&m).is_empty(), "{:?}", m.requests);
    let acts = badge_acts(&m);
    assert_eq!(acts.len(), 3, "{acts:#?}");
    assert!(acts[0].starts_with("meta set attention owner=supervisor claude read: "));
    assert_eq!(acts[1], "meta unset attention owner=supervisor");
    assert_eq!(acts[2], acts[0], "the same box, escalated again");
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
            ..auto(30)
        };
        let _ = watch_lines(&mut m, &opts);
        assert_eq!(*told.0.lock().unwrap(), want, "{:#?}", m.requests);
    }
}

#[test]
fn a_press_refused_or_warned_input_unread_reads_status_before_it_presses_again() {
    for (tag, reply, refused) in [
        (
            "stall-refused",
            err(
                "busy input-unread bytes=1 wait_ms=1500 input=pending (the program has not \
                 read input queued 1s ago; retry in a moment)",
            ),
            true,
        ),
        (
            "stall-warned",
            ok("OK seq=101 warn=input-unread input_bytes=1 input_wait_ms=1500\n"),
            false,
        ),
    ] {
        let mut m = fenced(vec![busy_screen(), bash_one_row()]);
        m.vanish_after = Some(4);
        m.key_replies.push_back(reply);
        m.input_after = vec![(1, INPUT_STALLED)];
        let (lines, code, journal) = watch_journaled(&mut m, tag);
        assert_eq!(writes(&m).len(), 1, "{tag}: {:?}", m.requests);
        assert_eq!(
            journal
                .iter()
                .any(|l| l.starts_with("REFUSED") && l.contains("ERR busy input-unread")),
            refused,
            "{tag}: {journal:#?}"
        );
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
        assert_eq!(after.get(read_at + 1), Some(&"status"), "{tag}: {after:#?}");
        assert!(
            journal.iter().any(|l| l.starts_with("FROZEN")),
            "{tag}: {journal:#?}"
        );
        ended_with_the_session(&lines, code);
    }
}

fn rendered(verb: &str, header: &str, body: &str) -> CtlReply {
    let framing = aterm_types::control_verbs::framing_of(verb, verb);
    crate::supervise::transport::render(verb, framing, header, body.as_bytes())
}

const WARNED_TURN: &str = "OK 1 turn submitted=1 status=settled seq=9 id=2 dur_ms=5 hash=00 \
    warn=input-unread input_bytes=2 input_wait_ms=1100";

#[test]
fn a_warning_is_read_off_the_first_line_the_server_wrote() {
    let key = rendered(
        "key",
        "OK seq=5 warn=input-unread input_bytes=1 input_wait_ms=1200",
        "",
    );
    assert!(key.warned_input_unread(), "{key:?}");
    let turn = rendered("turn", WARNED_TURN, "row\n");
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
        let mut m = Mock::new(vec![busy_screen(), ended, busy_screen()]);
        m.vanish_after = Some(3);
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
            ..auto(30)
        };
        let (lines, code) = watch_lines(&mut m, &opts);
        let journal = journal_lines(&path);
        let _ = std::fs::remove_dir_all(&dir);
        (m, lines, code, journal)
    };
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

#[test]
fn a_host_that_does_not_measure_input_is_probed_on_no_step() {
    for input in ["", "input=- input_bytes=- input_wait_ms=- fg_rss_mb=-"] {
        let mut m = box_that_sits();
        m.input = input;
        let (lines, code, journal) = watch_journaled(&mut m, "stall-none");
        assert_eq!(count(&m, "status"), 1, "input={input:?}: {:#?}", m.requests);
        let first_step = m
            .requests
            .iter()
            .position(|r| r.starts_with("await gone ^"))
            .expect("the wait on the box");
        assert!(
            !m.requests[first_step..].iter().any(|r| r == "status"),
            "input={input:?}: {:#?}",
            m.requests
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

#[test]
fn a_worker_already_stalled_when_the_loop_looks_is_neither_pressed_nor_badged() {
    for (screen, tag) in [
        (bash_one_row(), "stall-first-press"),
        (read_box(), "stall-first-box"),
    ] {
        let mut m = Mock::new(vec![busy_screen(), screen]);
        m.vanish_after = Some(4);
        m.input = INPUT_STALLED;
        let (lines, code, journal) = watch_journaled(&mut m, tag);
        assert!(writes(&m).is_empty(), "{tag}: {:?}", m.requests);
        assert!(badge_acts(&m).is_empty(), "{tag}: {:#?}", m.requests);
        assert!(
            !journal
                .iter()
                .any(|l| l.starts_with("APPROVED") || l.starts_with("ESCALATED")),
            "{tag}: {journal:#?}"
        );
        let frozen: Vec<&String> = journal.iter().filter(|l| l.starts_with("FROZEN")).collect();
        assert_eq!(frozen.len(), 1, "{tag}: {journal:#?}");
        assert!(
            frozen[0].ends_with(" attention=none"),
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

#[test]
fn a_continuation_over_a_worker_already_stalled_types_nothing() {
    let mut ended = rows(&[
        "⏺ Fixed the parser; the suite is green.",
        "",
        "✻ Worked for 3m 2s · done 4:24 PM",
        "",
    ]);
    ended.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    let mut m = Mock::new(vec![busy_screen(), ended]);
    m.vanish_after = Some(2);
    m.input = INPUT_STALLED;
    let (dir, path) = journal_file("stall-first-continue");
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        policy: SupervisorConfig::default(),
        ..auto(30)
    };
    let (lines, code) = watch_lines(&mut m, &opts);
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
    assert!(frozen.ends_with(" attention=none"), "{frozen}");
    assert!(
        badge_acts(&m).is_empty(),
        "nothing raised: {:#?}",
        m.requests
    );
    ended_with_the_session(&lines, code);
}

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
    let from = m.order.len();
    let (tape, mut taped) = memory_tape(m);
    let mut s = taped_session(&mut taped, &tape);
    s.set_approval_ledger(Some(dir.join("ledger.jsonl")));
    let header = hosted_header(&s, &opts);
    tape.core().reroot(header.roots.clone());
    let end = s.run_hosted(&opts, Arc::new(AtomicBool::new(false)), &mut out);
    drop(s);
    drop(taped);
    tape.finish(&crate::supervise::tape::replay::hosted_end(&end));
    let journal = journal_lines(&path);
    let text = String::from_utf8(out).expect("utf-8");
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    crate::supervise::tape::testing::assert_replays(&tape, &header, &sent(m, from), &lines);
    let _ = std::fs::remove_dir_all(&dir);
    (lines, end, journal)
}

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

fn before_the_badge(m: &Mock) -> &[String] {
    let at = m
        .requests
        .iter()
        .position(|r| r.starts_with("meta set"))
        .unwrap_or(m.requests.len());
    &m.requests[..at]
}

#[test]
fn the_windows_host_raises_no_badge_over_a_worker_frozen_under_its_point() {
    for (screen, tag) in [
        (question_screen(), "hosted-question"),
        (idle_asking(), "hosted-idle"),
    ] {
        let mut m = Mock::new(vec![busy_screen(), screen]);
        m.vanish_after = Some(4);
        m.input = INPUT_STALLED;
        let (lines, end, journal) = hosted_journaled(&mut m, tag);
        assert!(writes(&m).is_empty(), "{tag}: {:?}", m.requests);
        assert!(
            badge_acts(&m).is_empty(),
            "{tag}: no badge: {:#?}",
            m.requests
        );
        let escalated = journal
            .iter()
            .find(|l| l.starts_with("ESCALATED"))
            .unwrap_or_else(|| panic!("{tag}: ESCALATED: {journal:#?}"));
        assert!(
            escalated.ends_with(
                " attention=skipped: the same status says the worker is not reading its input"
            ),
            "{tag}: {escalated}"
        );
        let frozen: Vec<&String> = journal.iter().filter(|l| l.starts_with("FROZEN")).collect();
        assert_eq!(frozen.len(), 1, "{tag}: {journal:#?}");
        assert!(
            frozen[0]
                .ends_with(" input=stalled wait_ms=161000 bytes=1 rss_mb=39731 attention=none"),
            "{tag}: {}",
            frozen[0]
        );
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

#[test]
fn the_windows_host_withdraws_the_badge_of_a_worker_that_freezes_under_it() {
    let mut m = Mock::new(vec![busy_screen(), question_screen()]);
    m.vanish_after = Some(4);
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
    assert!(frozen[0].ends_with(" attention=OK"), "{}", frozen[0]);
    assert!(end.is_err(), "{end:?}");
    assert!(
        lines
            .last()
            .is_some_and(|l| l.starts_with("EXIT session gone (")),
        "{lines:?}"
    );
}

#[test]
fn a_wait_that_never_read_status_probes_and_holds() {
    let mut m = Mock::new(vec![busy_screen(), idle_screen()]);
    m.vanish_after = Some(3);
    m.input = INPUT_STALLED;
    let (lines, code, journal) = watch_journaled(&mut m, "stall-unread-idle");
    assert!(writes(&m).is_empty(), "{:?}", m.requests);
    assert!(badge_acts(&m).is_empty(), "{:#?}", m.requests);
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
    assert!(frozen.ends_with(" attention=none"), "{frozen}");
    ended_with_the_session(&lines, code);
}

#[test]
fn the_windows_host_asks_a_host_that_does_not_measure_input_once() {
    for input in ["", "input=- input_bytes=- input_wait_ms=- fg_rss_mb=-"] {
        let mut m = Mock::new(vec![busy_screen(), question_screen()]);
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

const RESTART: &str = "restart pid=4242 if-stalled quiet=120";

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
        ..auto(30)
    };
    opts.policy.stall_term_after_s = term_after_s;
    opts.policy.stall_term = stall_term;
    let (lines, _) = watch_lines(&mut m, &opts);
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
        warned[0].contains(" input=stalled wait_ms=660000 rss_mb=39731 ended=no "),
        "{}",
        warned[0]
    );
    assert_eq!(informed.len(), 1, "one record for the person: {informed:?}");
    assert!(
        informed[0].contains(" restart pid=4242 if-stalled` ")
            && informed[0].contains(crate::harness::resume::RELAUNCH_WORDS),
        "{}",
        informed[0]
    );
    let line = "claude --resume 5f1c2d3e-4b5a-4c6d-8e7f-0a1b2c3d4e5f";
    let long = format!(
        "claude --add-dir /{} --resume {}",
        "d".repeat(160),
        &line[16..]
    );
    for after in [
        AfterRestart::Command(line.to_string()),
        AfterRestart::Command(long),
    ] {
        let (_, _, informed) =
            remedy_run_as(INPUT_STALLED_LONG, true, default, false, after, |_| {});
        assert_eq!(informed.len(), 1, "{informed:?}");
        assert!(
            informed[0].contains(&format!("`{line}`"))
                && !informed[0].contains(crate::harness::resume::RELAUNCH_WORDS),
            "{}",
            informed[0]
        );
    }
}

#[test]
fn a_worker_frozen_past_the_bound_is_ended_for_its_relaunch() {
    let default = SupervisorConfig::default().stall_term_after_s;
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

    let (signals, lines) = remedy_run(INPUT_STALLED_LONG, true, default, |m| {
        m.hand = "lease:@s-7";
    });
    assert!(signals.is_empty(), "{signals:?} {lines:#?}");
}

fn input_stopped(aged: bool) -> &'static str {
    if aged {
        "input=stopped input_bytes=1 input_wait_ms=660000 fg_rss_mb=-"
    } else {
        "input=stopped input_bytes=1 input_wait_ms=161000 fg_rss_mb=-"
    }
}

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

#[test]
fn tier1_the_real_remedy_signals_exactly_where_the_model_terms() {
    use std::collections::{BTreeMap, BTreeSet, VecDeque};
    let m = aterm_models_harness::supervisor_stall_remedy_model();
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([m.init_state()]);
    let mut configs = BTreeMap::new();
    while let Some(st) = queue.pop_front() {
        if !seen.insert(st.clone()) {
            continue;
        }
        if st["stalled"] > 0 && st["termed"] == 0 {
            let key = (
                st["stalled"],
                st["aged"],
                st["off"],
                st["hand"],
                st["host"],
                st["beside"],
                st["rest"],
            );
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
    assert_eq!(configs.len(), 128, "every configuration is reachable");
    assert_eq!(
        configs.values().filter(|t| **t).count(),
        4,
        "four configurations term (beside or not, its record at rest or not): {configs:?}"
    );
    for (&(stalled, aged, off, hand, host, beside, rest), &term) in &configs {
        let input = match (stalled, aged) {
            (1, 1) => INPUT_STALLED_LONG,
            (1, _) => INPUT_STALLED,
            (_, a) => input_stopped(a == 1),
        };
        let bound = if off == 1 { 0 } else { 600 };
        let tweak = |mk: &mut Mock| {
            if hand == 1 {
                mk.human_ms = Some(1_000);
            }
        };
        let run = |stall_term: bool| {
            if beside == 1 {
                beside_remedy_run(input, host == 1, bound, stall_term, rest == 1, tweak)
            } else if stall_term {
                let (signals, lines) = remedy_run(input, host == 1, bound, tweak);
                (signals, lines, Vec::new())
            } else {
                warn_run(input, host == 1, bound, tweak)
            }
        };
        let (signals, lines, _) = run(true);
        let want: &[&str] = if term { &[RESTART] } else { &[] };
        assert_eq!(
            signals, want,
            "stalled={stalled} aged={aged} off={off} hand={hand} host={host} beside={beside} \
             rest={rest}: {lines:#?}"
        );
        let (signals, lines, informed) = run(false);
        let warned = lines
            .iter()
            .filter(|l| l.starts_with("STALL-WARNED"))
            .count();
        assert!(signals.is_empty(), "the default never signals: {signals:?}");
        assert_eq!(
            (warned, informed.len()),
            if term { (1, 1) } else { (0, 0) },
            "stalled={stalled} aged={aged} off={off} hand={hand} host={host} beside={beside} \
             rest={rest}: {lines:#?}"
        );
    }
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
    let mut st = buggy.init_state();
    for action in ["BesideFlips", "Freeze", "Age"] {
        assert!(buggy.fire(action, &mut st), "{action} at {st:?}");
    }
    assert!(
        buggy.action_enabled("WaitsForTheRest", &st),
        "a remedy that waits on the record"
    );
    let (signals, ..) = beside_remedy_run(INPUT_STALLED_LONG, true, 600, true, false, |_| {});
    assert_eq!(signals, [RESTART], "beside, its record busy");
}

/// A host the loop runs beside a proven self-claim: it relaunches the agent
/// the explicit restart ends (its crash rule, pid 4242) or not, says whether
/// the agent's own record names an idle point ([`IdleHost::beside_idle`]),
/// measures `memory`, counts the idle reads and keeps what it was told for a
/// person.
#[derive(Debug)]
struct Beside {
    idle: bool,
    /// The record reads idle at every other read only (a stale `idle` the
    /// next read takes back): an idle point that never stands.
    flickers: bool,
    relaunches: bool,
    memory: Option<crate::supervise::memory::AgentMemory>,
    asked: std::sync::atomic::AtomicUsize,
    informed: Mutex<Vec<String>>,
}

impl Beside {
    fn new(idle: bool, memory: Option<crate::supervise::memory::AgentMemory>) -> Arc<Self> {
        Arc::new(Self {
            idle,
            flickers: false,
            relaunches: true,
            memory,
            asked: std::sync::atomic::AtomicUsize::new(0),
            informed: Mutex::new(Vec::new()),
        })
    }

    fn idle_reads(&self) -> usize {
        self.asked.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl IdleHost for Beside {
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
        self.relaunches
    }
    fn agent_pid(&self) -> Option<u32> {
        Some(4242)
    }
    fn beside_idle(&self) -> bool {
        let n = self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.idle && (!self.flickers || n.is_multiple_of(2))
    }
    fn memory(&self) -> Option<crate::supervise::memory::AgentMemory> {
        self.memory
    }
    fn inform(&self, text: &str) {
        self.informed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(text.to_string());
    }
}

fn beside_remedy_run(
    input: &'static str,
    relaunches: bool,
    term_after_s: u32,
    stall_term: bool,
    rest: bool,
    tweak: impl FnOnce(&mut Mock),
) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut m = box_that_sits();
    m.input_after = vec![(2, input)];
    tweak(&mut m);
    let host = Arc::new(Beside {
        relaunches,
        ..Arc::into_inner(Beside::new(rest, None)).expect("one owner")
    });
    let mut opts = SuperviseOpts {
        idle_host: Some(host.clone() as Arc<dyn IdleHost>),
        ..auto(30)
    };
    opts.policy.stall_term_after_s = term_after_s;
    opts.policy.stall_term = stall_term;
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_beside(Some("codex-alab@4242".to_string()));
    });
    let signals = m
        .requests
        .iter()
        .filter(|r| r.starts_with("signal") || r.starts_with("restart"))
        .cloned()
        .collect();
    let informed = host
        .informed
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    (signals, lines, informed)
}

#[test]
fn beside_a_proven_self_claim_the_stalls_remedy_stands_and_nothing_is_typed() {
    let default = SupervisorConfig::default().stall_term_after_s;
    let run_idle = |beside: bool, idle: bool| {
        let mut m = box_that_sits();
        m.input_after = vec![(2, INPUT_STALLED_LONG)];
        if !beside {
            m.claim_replies = [err("busy supervisor=watcher%40elsewhere")].into();
        }
        let host = Beside::new(idle, None);
        let asked = Arc::clone(&host);
        let mut opts = SuperviseOpts {
            idle_host: Some(host as Arc<dyn IdleHost>),
            ..auto(30)
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
        let idle_reads = asked.idle_reads();
        (remedies, claims, typed, badge_acts(&m), lines, idle_reads)
    };
    let run = |beside: bool| {
        let (remedies, claims, typed, badges, lines, _) = run_idle(beside, true);
        (remedies, claims, typed, badges, lines)
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
    let (remedies, claims, typed, _, lines, idle_reads) = run_idle(true, false);
    assert_eq!(remedies, [RESTART], "frozen mid-turn: {lines:#?}");
    assert!(claims.is_empty(), "{claims:?}");
    assert_eq!(typed, 0, "{lines:#?}");
    assert_eq!(idle_reads, 0, "the stall reads no record's rest");
}

fn memory_in_trouble() -> crate::supervise::memory::AgentMemory {
    use crate::supervise::memory::{AgentMemory, Pressure};
    const GIB: u64 = 1024;
    AgentMemory {
        footprint_mib: 200 * GIB,
        physical_mib: Some(128 * GIB),
        available_mib: Some(4 * GIB),
        used_mib: Some(120 * GIB),
        pressure: Some(Pressure::Critical),
        swap_kib_s: None,
        largest_other_mib: None,
    }
}

const MEMORY_RESTART: &str = "restart pid=4242 quiet=120";

#[test]
fn beside_a_proven_self_claim_the_memory_restart_waits_for_the_records_idle_point() {
    use crate::supervise::memory::AgentMemory;
    let run = |beside: bool, idle: bool, memory: AgentMemory, relaunch: bool| {
        let mut m = box_that_sits();
        m.key_gates.push((1, usize::MAX));
        let host = Beside::new(idle, Some(memory));
        let asked = Arc::clone(&host);
        let mut opts = SuperviseOpts {
            idle_host: Some(host as Arc<dyn IdleHost>),
            ..auto(30)
        };
        opts.policy.relaunch = relaunch;
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            if beside {
                s.set_beside(Some("codex-alab@4242".to_string()));
                s.setup.beside_idle_min = Duration::ZERO;
            }
        });
        let remedies: Vec<String> = m
            .requests
            .iter()
            .filter(|r| r.starts_with("signal") || r.starts_with("restart"))
            .cloned()
            .collect();
        let claims = m
            .requests
            .iter()
            .filter(|r| r.starts_with("meta set supervisor"))
            .count();
        let idle_reads = asked.idle_reads();
        (remedies, claims, writes(&m).len(), lines, idle_reads)
    };
    let trouble = memory_in_trouble();
    let (remedies, claims, typed, lines, _) = run(true, true, trouble, true);
    assert_eq!(
        remedies,
        [MEMORY_RESTART],
        "once, at the idle point: {lines:#?}"
    );
    assert_eq!(claims, 0, "nothing claimed beside");
    assert_eq!(typed, 0, "nothing typed: {lines:#?}");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("RESTARTED ") && l.contains(" memory ")),
        "{lines:#?}"
    );
    let (remedies, _, typed, lines, idle_reads) = run(true, false, trouble, true);
    assert!(remedies.is_empty(), "not at its idle point: {lines:#?}");
    assert_eq!(typed, 0, "{lines:#?}");
    assert!(idle_reads >= 2, "asked again at each step: {idle_reads}");
    let (remedies, ..) = run(true, true, trouble, false);
    assert!(remedies.is_empty(), "relaunch off: no restart");
    let (remedies, _, typed, lines, idle_reads) = {
        let mut m = box_that_sits();
        m.key_gates.push((1, usize::MAX));
        m.vanish_after = Some(8);
        let host = Arc::new(Beside {
            flickers: true,
            ..Arc::into_inner(Beside::new(true, Some(trouble))).expect("one owner")
        });
        let asked = Arc::clone(&host);
        let opts = SuperviseOpts {
            idle_host: Some(host as Arc<dyn IdleHost>),
            ..auto(30)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_beside(Some("codex-alab@4242".to_string()));
            s.setup.beside_idle_min = Duration::ZERO;
        });
        let remedies: Vec<String> = m
            .requests
            .iter()
            .filter(|r| r.starts_with("signal") || r.starts_with("restart"))
            .cloned()
            .collect();
        (remedies, 0, writes(&m).len(), lines, asked.idle_reads())
    };
    assert!(remedies.is_empty(), "an idle that never stands: {lines:#?}");
    assert_eq!(typed, 0, "{lines:#?}");
    assert!(idle_reads >= 4, "asked at each step: {idle_reads}");
    let spare = AgentMemory {
        pressure: Some(crate::supervise::memory::Pressure::Normal),
        ..trouble
    };
    let (remedies, ..) = run(true, true, spare, true);
    assert!(remedies.is_empty(), "a warning only: no restart");
    let (remedies, _, _, lines, idle_reads) = run(false, true, trouble, true);
    assert!(
        !remedies.contains(&MEMORY_RESTART.to_string()),
        "no self-claim: not this path: {lines:#?}"
    );
    assert_eq!(idle_reads, 0, "no record read off a self-claim");
}

#[test]
fn under_a_hold_beside_a_proven_self_claim_the_remedies_stand_and_nothing_is_typed() {
    let halted = || err("halted reason=owner origin=local");
    let run = |memory: bool, replies: Vec<CtlReply>| {
        let mut m = box_that_sits();
        m.key_gates.push((1, usize::MAX));
        m.vanish_after = Some(8);
        m.hold = "1";
        m.hold_lifts_after = Some(4);
        m.restart_replies = replies.into();
        let host = if memory {
            Beside::new(true, Some(memory_in_trouble()))
        } else {
            m.input_after = vec![(2, INPUT_STALLED_LONG)];
            Beside::new(true, None)
        };
        let mut opts = SuperviseOpts {
            idle_host: Some(host as Arc<dyn IdleHost>),
            ..auto(30)
        };
        opts.policy.stall_term_after_s = SupervisorConfig::default().stall_term_after_s;
        opts.policy.stall_term = true;
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_beside(Some("codex-alab@4242".to_string()));
            s.setup.beside_idle_min = Duration::ZERO;
        });
        let remedies: Vec<String> = m
            .requests
            .iter()
            .filter(|r| r.starts_with("signal") || r.starts_with("restart"))
            .cloned()
            .collect();
        let claims = m
            .requests
            .iter()
            .filter(|r| r.starts_with("meta set supervisor"))
            .count();
        (remedies, claims, writes(&m).len(), lines)
    };
    for (memory, act) in [(false, RESTART), (true, MEMORY_RESTART)] {
        let (remedies, claims, typed, lines) = run(memory, vec![halted(), halted()]);
        assert_eq!(
            remedies,
            [act, act, act],
            "memory={memory}: asked again through the halt, made once it lifts: {lines:#?}"
        );
        assert_eq!(claims, 0, "memory={memory}: nothing claimed");
        assert_eq!(typed, 0, "memory={memory}: nothing typed: {lines:#?}");
        let refused = lines
            .iter()
            .filter(|l| l.starts_with("RESTART-REFUSED ") && l.contains("ERR halted"))
            .count();
        assert_eq!(refused, 2, "memory={memory}: {lines:#?}");
        assert_eq!(
            lines.iter().filter(|l| l.starts_with("RESTARTED ")).count(),
            1,
            "memory={memory}: {lines:#?}"
        );
        let (remedies, _, typed, lines) = run(
            memory,
            vec![
                err("restart unavailable: driver-lease"),
                err("restart unavailable: driver-turn"),
            ],
        );
        assert_eq!(
            remedies,
            [act, act, act],
            "memory={memory}: asked again through a lease and a turn: {lines:#?}"
        );
        assert_eq!(typed, 0, "{lines:#?}");
        let (remedies, _, typed, lines) =
            run(memory, vec![err("restart unavailable: not-relaunchable")]);
        assert_eq!(remedies, [act], "memory={memory}: once: {lines:#?}");
        assert_eq!(typed, 0, "{lines:#?}");
    }
}

struct IdleMemoryReview {
    unattended: bool,
}

impl Review for IdleMemoryReview {
    fn approved(&mut self, _seq: u64, _command: &str) -> Result<(), String> {
        Ok(())
    }
    fn review(&mut self, _turn: &Turn) -> Result<bool, String> {
        Ok(true)
    }
    fn say(&mut self, line: &str) -> Result<(), String> {
        crate::supervise::ledger::assert_marked(line);
        Ok(())
    }
    fn note(&mut self, line: &str) {
        crate::supervise::ledger::assert_marked(line);
    }
    fn unattended(&self) -> bool {
        self.unattended
    }
}

fn memory_text_at(seq: u64) -> VecDeque<CtlReply> {
    (0..16)
        .map(|_| {
            ok(&format!(
                "{{\"rows\":[\"idle\"],\"cursor\":{{\"row\":0,\"col\":0}},\"seq\":{seq}}}\n"
            ))
        })
        .collect()
}

fn actionable_memory() -> Option<crate::supervise::memory::MemoryOver> {
    const GIB: u64 = 1024;
    Some(crate::supervise::memory::MemoryOver {
        footprint_mib: 200 * GIB,
        ceiling_mib: 48 * GIB,
        available_mib: Some(4 * GIB),
        strain: Some(crate::supervise::memory::Strain::Critical),
        dominant: true,
    })
}

#[test]
fn a_beside_memory_restart_waits_for_elapsed_observation_not_just_two_reads() {
    let run = |second: Duration| {
        let mut m = Mock::new(vec![vec!["idle".to_string()]]);
        m.verb_replies.insert("text", memory_text_at(7));
        let mut s = session(&mut m, None);
        s.set_beside(Some("codex-alab@4242".to_string()));
        s.stall_host = Some(Beside::new(true, None) as Arc<dyn IdleHost>);
        let opts = auto(30);
        let mut review = IdleMemoryReview { unattended: true };
        let first = Instant::now();
        for now in [first, first + second] {
            s.beside_memory_remedy_at(actionable_memory(), 7, || now, &opts, &mut review)
                .expect("scripted memory remedy");
        }
        count(s.ctl, "restart")
    };
    assert_eq!(run(WAIT_STEP), 1, "an eligible full observation can act");
    assert_eq!(
        run(Duration::ZERO),
        0,
        "two samples at one virtual instant establish no elapsed observation"
    );
}

#[test]
fn a_beside_memory_restart_discards_a_candidate_across_ineligibility() {
    let mut observed = Vec::new();
    for why in ["policy", "host policy", "attended", "host absent", "holder"] {
        let mut m = Mock::new(vec![vec!["idle".to_string()]]);
        m.verb_replies.insert("text", memory_text_at(7));
        let mut s = session(&mut m, None);
        s.set_beside(Some("codex-alab@4242".to_string()));
        let host = Beside::new(true, None) as Arc<dyn IdleHost>;
        s.stall_host = Some(Arc::clone(&host));
        let mut opts = auto(30);
        let mut review = IdleMemoryReview { unattended: true };
        let first = Instant::now();
        s.beside_memory_remedy_at(actionable_memory(), 7, || first, &opts, &mut review)
            .expect("arm an eligible observation");
        match why {
            "policy" => opts.policy.relaunch = false,
            "host policy" => {
                s.stall_host = Some(Arc::new(Beside {
                    relaunches: false,
                    ..Arc::into_inner(Beside::new(true, None)).expect("one owner")
                }) as Arc<dyn IdleHost>);
            }
            "attended" => review.unattended = false,
            "host absent" => s.stall_host = None,
            "holder" => s.set_beside(Some("another-self-claim@4242".to_string())),
            _ => unreachable!(),
        }
        if why != "holder" {
            s.beside_memory_remedy_at(
                actionable_memory(),
                7,
                || first + WAIT_STEP,
                &opts,
                &mut review,
            )
            .expect("an ineligible sample is inert");
        }
        assert_eq!(count(s.ctl, "restart"), 0, "{why}: no act while ineligible");
        opts.policy.relaunch = true;
        review.unattended = true;
        s.stall_host = Some(host);
        s.beside_memory_remedy_at(
            actionable_memory(),
            7,
            || first + WAIT_STEP,
            &opts,
            &mut review,
        )
        .expect("the first newly eligible sample");
        observed.push((why, count(s.ctl, "restart")));
    }
    assert!(
        observed.iter().all(|(_, n)| *n == 0),
        "a new eligible observation starts over: {observed:?}"
    );
}

#[test]
fn a_beside_memory_restart_rechecks_the_current_screen_sequence() {
    let mut m = Mock::new(vec![vec!["idle".to_string()]]);
    m.verb_replies.insert("text", memory_text_at(7));
    let mut s = session(&mut m, None);
    s.set_beside(Some("codex-alab@4242".to_string()));
    s.stall_host = Some(Beside::new(true, None) as Arc<dyn IdleHost>);
    let opts = auto(30);
    let mut review = IdleMemoryReview { unattended: true };
    let first = Instant::now();
    s.beside_memory_remedy_at(actionable_memory(), 7, || first, &opts, &mut review)
        .expect("first idle observation");
    for current in [8, 1] {
        s.ctl.verb_replies.insert("text", memory_text_at(current));
        s.beside_memory_remedy_at(
            actionable_memory(),
            7,
            || first + WAIT_STEP,
            &opts,
            &mut review,
        )
        .expect("read the current content sequence");
    }
    assert_eq!(
        count(s.ctl, "restart"),
        0,
        "the wait's frozen seq is not fresh"
    );
}
