// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::atomic::AtomicUsize;

use super::*;

const REC_ATTRIBUTION: &str = "2026-08-31 15:58:45.494008-0700 0x8de257   Default     \
    0x1579c5c            66389  0    tccd: [com.apple.TCC:access] AUTHREQ_ATTRIBUTION: \
    msgID=378.385, attribution={responsible={TCCDProcess: identifier=com.example.term, \
    pid=4059, auid=501, euid=501, responsible_path=/Applications/Example.app/Contents/\
    MacOS/Example, binary_path=/Applications/Example.app/Contents/MacOS/Example}, \
    accessing={TCCDProcess: identifier=com.apple.mdfind, pid=18584, auid=501, euid=501, \
    binary_path=/usr/bin/mdfind}, requesting={TCCDProcess: identifier=com.apple.mds, \
    pid=378, auid=0, euid=0, binary_path=/System/Library/Frameworks/CoreServices.\
    framework/Versions/A/Frameworks/Metadata.framework/Versions/A/Support/mds}, },";

const REC_ATTRIBUTION_SELF: &str = "2026-08-31 15:30:50.039 Df tccd[66444:8d06f0] \
    [com.apple.TCC:access] AUTHREQ_ATTRIBUTION: msgID=3956.1, \
    attribution={requesting={TCCDProcess: identifier=com.apple.PerfPowerServices, \
    pid=3956, auid=0, euid=0, binary_path=/usr/libexec/PerfPowerServices}, },";

const REC_SUBJECT: &str = "2026-08-31 15:30:50.049 Df tccd[66444:8d0f0d] \
    [com.apple.TCC:access] AUTHREQ_SUBJECT: msgID=378.385, \
    subject=/usr/bin/mdfind,";

const REC_CTX: &str = "2026-08-31 15:58:45.493995-0700 0x8de257   Default     \
    0x1579c5c            66389  0    tccd: [com.apple.TCC:access] AUTHREQ_CTX: \
    msgID=378.385, function=<private>, service=kTCCServiceSystemPolicyDocumentsFolder, \
    preflight=no, query=1, client_dict=(null), daemon_dict=<private>";

const REC_CTX_PREFLIGHT: &str = "2026-08-31 15:58:45.493995-0700 0x8de257   Default     \
    0x1579c5c            66389  0    tccd: [com.apple.TCC:access] AUTHREQ_CTX: \
    msgID=378.385, function=<private>, service=kTCCServiceAddressBook, preflight=yes, \
    query=1, client_dict=(null), daemon_dict=<private>";

const REC_RESULT: &str = "2026-08-31 15:41:28.194 Df tccd[66389:8d871a] \
    [com.apple.TCC:access] AUTHREQ_RESULT: msgID=378.385, authValue=2, authReason=11, \
    authVersion=1, desired_auth=0, error=(null),";

#[test]
fn the_recorded_log_lines_parse_into_their_events() {
    let cases: &[(&str, TccEvent)] = &[
        (
            REC_ATTRIBUTION,
            TccEvent::Attribution {
                msg_id: "378.385".to_owned(),
                accessing_pid: Some(18584),
                requesting_pid: Some(378),
            },
        ),
        (
            REC_ATTRIBUTION_SELF,
            TccEvent::Attribution {
                msg_id: "3956.1".to_owned(),
                accessing_pid: None,
                requesting_pid: Some(3956),
            },
        ),
        (
            REC_SUBJECT,
            TccEvent::Subject {
                msg_id: "378.385".to_owned(),
                subject: "/usr/bin/mdfind".to_owned(),
            },
        ),
        (
            REC_CTX,
            TccEvent::Ctx {
                msg_id: "378.385".to_owned(),
                service: "kTCCServiceSystemPolicyDocumentsFolder".to_owned(),
                preflight: Some(false),
            },
        ),
        (
            REC_CTX_PREFLIGHT,
            TccEvent::Ctx {
                msg_id: "378.385".to_owned(),
                service: "kTCCServiceAddressBook".to_owned(),
                preflight: Some(true),
            },
        ),
        (
            REC_RESULT,
            TccEvent::Result {
                msg_id: "378.385".to_owned(),
            },
        ),
    ];
    for (line, expected) in cases {
        assert_eq!(
            parse_line(line).as_ref(),
            Some(expected),
            "recorded line must parse: {line}"
        );
    }
}

#[test]
fn a_non_authreq_line_is_not_a_format_change() {
    for line in [
        "",
        "2026-08-31 10:22:31 tccd: [com.apple.TCC:access] Received request",
        "some unrelated process line",
    ] {
        assert_eq!(parse_line(line), None, "not an AUTHREQ line: {line}");
    }
}

#[test]
fn a_recognized_kind_with_missing_fields_is_unparsed_never_guessed() {
    let mangled = [
        "tccd: AUTHREQ_ATTRIBUTION: msgID=475.2921, attribution={accessing={}}",
        "tccd: AUTHREQ_ATTRIBUTION: attribution={accessing={pid=17}}",
        "tccd: AUTHREQ_SUBJECT: msgID=475.2921,",
        "tccd: AUTHREQ_CTX: msgID=475.2921, preflight=no",
        "tccd: AUTHREQ_RESULT: authValue=2",
    ];
    for line in mangled {
        assert_eq!(
            parse_line(line),
            Some(TccEvent::Unparsed),
            "a mangled recognized kind must be Unparsed: {line}"
        );
    }
}

#[test]
fn the_preflight_bit_is_read_on_its_recorded_spellings() {
    let yes = "tccd: AUTHREQ_CTX: msgID=1.1, service=kTCCServiceX, preflight=yes";
    assert_eq!(
        parse_line(yes),
        Some(TccEvent::Ctx {
            msg_id: "1.1".to_owned(),
            service: "kTCCServiceX".to_owned(),
            preflight: Some(true),
        })
    );
    let absent = "tccd: AUTHREQ_CTX: msgID=1.1, service=kTCCServiceX";
    assert_eq!(
        parse_line(absent),
        Some(TccEvent::Ctx {
            msg_id: "1.1".to_owned(),
            service: "kTCCServiceX".to_owned(),
            preflight: None,
        }),
        "an absent field is None, never a guessed false"
    );
}

fn feed(table: &mut PendingTable, line: &str, now: Instant) {
    let event = parse_line(line).expect("recorded line parses");
    table.note(event, now);
}

#[test]
fn an_unanswered_ctx_with_an_accessing_pid_is_a_pending_prompt() {
    let t0 = Instant::now();
    let mut table = PendingTable::new();
    feed(&mut table, REC_CTX, t0);
    feed(&mut table, REC_ATTRIBUTION, t0);
    feed(&mut table, REC_SUBJECT, t0);

    assert!(
        table
            .pending_prompts(t0, PENDING_PROMPT_THRESHOLD)
            .is_empty(),
        "a request that just opened is not yet a prompt"
    );

    let later = t0 + PENDING_PROMPT_THRESHOLD + Duration::from_secs(1);
    let prompts = table.pending_prompts(later, PENDING_PROMPT_THRESHOLD);
    assert_eq!(prompts.len(), 1, "exactly one open request");
    assert_eq!(prompts[0].accessing_pid, 18584);
    assert_eq!(prompts[0].pid_source, PidSource::Accessing);
    assert_eq!(prompts[0].subject, Some("/usr/bin/mdfind"));
}

#[test]
fn a_result_closes_the_request_and_no_prompt_is_reported() {
    let t0 = Instant::now();
    let mut table = PendingTable::new();
    feed(&mut table, REC_CTX, t0);
    feed(&mut table, REC_ATTRIBUTION, t0);
    feed(&mut table, REC_RESULT, t0 + Duration::from_secs(1));
    let later = t0 + PENDING_PROMPT_THRESHOLD + Duration::from_secs(5);
    assert!(
        table
            .pending_prompts(later, PENDING_PROMPT_THRESHOLD)
            .is_empty()
    );
    assert!(table.is_empty(), "the entry is gone, not merely hidden");
}

#[test]
fn a_ctx_without_an_accessing_pid_is_never_a_prompt() {
    let t0 = Instant::now();
    let mut table = PendingTable::new();
    feed(&mut table, REC_CTX, t0);
    let later = t0 + Duration::from_secs(60);
    assert!(
        table
            .pending_prompts(later, PENDING_PROMPT_THRESHOLD)
            .is_empty()
    );
    assert_eq!(table.len(), 1, "still tracked, just not a verdict");
}

#[test]
fn a_request_with_no_known_service_is_never_a_prompt() {
    let t0 = Instant::now();
    let mut table = PendingTable::new();
    feed(&mut table, REC_ATTRIBUTION, t0);
    let later = t0 + PENDING_PROMPT_THRESHOLD + Duration::from_secs(1);
    assert!(
        table
            .pending_prompts(later, PENDING_PROMPT_THRESHOLD)
            .is_empty()
    );
    assert_eq!(
        table.next_crossing(t0, PENDING_PROMPT_THRESHOLD),
        None,
        "and it arms no deadline"
    );
    assert_eq!(table.len(), 1, "still tracked, just not a verdict");
}

#[test]
fn a_preflight_request_is_never_a_pending_prompt() {
    let t0 = Instant::now();
    let mut table = PendingTable::new();
    feed(
        &mut table,
        "tccd: AUTHREQ_CTX: msgID=9.9, service=kTCCServiceX, preflight=yes",
        t0,
    );
    feed(
        &mut table,
        "tccd: AUTHREQ_ATTRIBUTION: msgID=9.9, attribution={accessing={pid=1234}}",
        t0,
    );
    let later = t0 + Duration::from_secs(60);
    assert!(
        table
            .pending_prompts(later, PENDING_PROMPT_THRESHOLD)
            .is_empty()
    );
}

#[test]
fn the_table_is_bounded_and_evicts_the_oldest() {
    let t0 = Instant::now();
    let mut table = PendingTable::new();
    for i in 0..(PENDING_CAP + 32) {
        feed(
            &mut table,
            &format!("tccd: AUTHREQ_CTX: msgID=1.{i}, service=kTCCServiceX, preflight=no"),
            t0,
        );
    }
    assert_eq!(table.len(), PENDING_CAP, "capped, never grown");
}

#[test]
fn an_old_entry_expires_rather_than_standing_as_evidence() {
    let t0 = Instant::now();
    let mut table = PendingTable::new();
    feed(&mut table, REC_CTX, t0);
    feed(&mut table, REC_ATTRIBUTION, t0);
    table.expire(
        t0 + PENDING_MAX_AGE - Duration::from_secs(1),
        PENDING_MAX_AGE,
    );
    assert_eq!(table.len(), 1, "not yet old enough");
    table.expire(
        t0 + PENDING_MAX_AGE + Duration::from_secs(1),
        PENDING_MAX_AGE,
    );
    assert!(table.is_empty(), "expired");
}

#[test]
fn owning_session_matches_the_pid_or_its_process_group_and_nothing_else() {
    let sessions = &[(7u64, 4711i32), (9, 4802)];
    let cases: &[(i32, Option<i32>, Option<u64>, &str)] = &[
        (4711, None, Some(7), "the shell pid itself"),
        (51234, Some(4802), Some(9), "a child in a session's group"),
        (51234, Some(9999), None, "a group we do not own"),
        (51234, None, None, "no group, no match"),
        (
            0,
            Some(4711),
            None,
            "a nonsense pid is refused before the join",
        ),
        (-1, Some(4711), None, "a negative pid is refused too"),
        (
            4802,
            Some(1),
            Some(9),
            "the pid wins over an unrelated group",
        ),
    ];
    for (pid, pgid, expected, why) in cases {
        assert_eq!(owning_session(*pid, *pgid, sessions), *expected, "{why}");
    }
}

#[test]
fn the_short_service_name_drops_the_prefix() {
    let cases: &[(&str, &str)] = &[
        (
            "kTCCServiceSystemPolicyDocumentsFolder",
            "SystemPolicyDocumentsFolder",
        ),
        ("SystemPolicyAllFiles", "SystemPolicyAllFiles"),
        ("  kTCCServiceCamera ", "Camera"),
        ("kTCCService", ""),
        ("", ""),
    ];
    for (service, expected) in cases {
        assert_eq!(short_service(service), *expected);
    }
}

#[test]
fn unavailable_off_and_ok_are_three_distinct_values() {
    assert_eq!(ObserverAvailability::Off.as_str(), "off");
    assert_eq!(ObserverAvailability::Ok.as_str(), "ok");
    for reason in [
        UnavailableReason::Inert,
        UnavailableReason::UnsupportedPlatform,
        UnavailableReason::NoLogAccess,
        UnavailableReason::LogToolUnavailable,
        UnavailableReason::StreamEnded,
        UnavailableReason::FormatUnrecognized,
    ] {
        let value = ObserverAvailability::Unavailable(reason);
        assert_eq!(value.as_str(), "unavailable");
        assert_ne!(value.as_str(), ObserverAvailability::Off.as_str());
        assert_ne!(value.as_str(), ObserverAvailability::Ok.as_str());
        assert_eq!(value.reason(), Some(reason));
        assert!(!reason.as_str().is_empty());
    }
    assert_eq!(ObserverAvailability::Off.reason(), None);
    assert_eq!(ObserverAvailability::Ok.reason(), None);
}

#[test]
fn a_fresh_observer_is_off_and_inert() {
    let observer = ObserverState::default();
    assert_eq!(observer.availability(), ObserverAvailability::Off);
    assert!(!observer.probe_is_live());
    assert!(!observer.is_live());
    assert_eq!(observer.pending_len(), 0);
}

#[test]
fn a_disabled_observer_starts_nothing() {
    let pokes = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&pokes);
    let mut observer = ObserverState::inert();
    assert_eq!(
        observer.start(ObserverMode::Off, move || {
            counter.fetch_add(1, Ordering::Relaxed);
        }),
        StartOutcome::Disabled
    );
    assert_eq!(observer.availability(), ObserverAvailability::Off);
    assert!(!observer.is_live());
    assert_eq!(pokes.load(Ordering::Relaxed), 0, "nothing was posted");
}

#[test]
fn the_inert_arm_degrades_to_unavailable_and_never_to_a_verdict() {
    let mut observer = ObserverState::inert();
    assert_eq!(
        observer.start(ObserverMode::Full, || {}),
        StartOutcome::Started
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    while observer.availability() == ObserverAvailability::Off && Instant::now() < deadline {
        observer.drain(Instant::now());
        aterm_uds::deadline::poll_yield();
    }
    assert_eq!(
        observer.availability(),
        ObserverAvailability::Unavailable(UnavailableReason::Inert)
    );
}

#[test]
fn a_second_start_while_live_is_a_no_op() {
    let mut observer = ObserverState::inert();
    assert_eq!(
        observer.start(ObserverMode::Full, || {}),
        StartOutcome::Started
    );
    assert_eq!(
        observer.start(ObserverMode::Full, || {}),
        StartOutcome::AlreadyLive
    );
    observer.stop();
    assert!(!observer.is_live());
}

#[test]
fn reconcile_restarts_only_on_a_changed_mode() {
    let mut observer = ObserverState::inert();
    assert_eq!(
        observer.reconcile(ObserverMode::Off, || {}),
        None,
        "a fresh observer already has what Off asks for"
    );
    assert_eq!(
        observer.reconcile(ObserverMode::Full, || {}),
        Some(StartOutcome::Started)
    );
    assert_eq!(observer.requested(), ObserverMode::Full);
    let deadline = Instant::now() + Duration::from_secs(60);
    while observer.is_live() && Instant::now() < deadline {
        observer.drain(Instant::now());
        aterm_uds::deadline::poll_yield();
    }
    assert!(!observer.is_live(), "the inert worker ended by itself");
    assert_eq!(
        observer.availability(),
        ObserverAvailability::Unavailable(UnavailableReason::Inert)
    );
    assert_eq!(
        observer.reconcile(ObserverMode::Full, || {}),
        None,
        "an ended stream is not respawned by the same wish"
    );
    assert!(!observer.is_live());
    assert_eq!(
        observer.reconcile(ObserverMode::Sentinel, || {}),
        Some(StartOutcome::Started),
        "a changed wish starts the new mode"
    );
    assert!(observer.is_live());
    assert_eq!(
        observer.reconcile(ObserverMode::Off, || {}),
        Some(StartOutcome::Disabled)
    );
    assert!(!observer.is_live());
    assert_eq!(observer.requested(), ObserverMode::Off);
    assert_eq!(observer.availability(), ObserverAvailability::Off);
}

#[test]
fn the_running_mode_is_set_by_started_and_cleared_by_an_end_or_a_stop() {
    let now = Instant::now();
    let mut observer = ObserverState::inert();
    assert_eq!(
        observer.start(ObserverMode::Full, || {}),
        StartOutcome::Started
    );
    assert_eq!(
        observer.running(),
        ObserverMode::Off,
        "nothing runs until the worker says so"
    );
    observer.feed_for_test(ObserverEvent::Started, now);
    assert_eq!(observer.running(), ObserverMode::Full);
    let deadline = Instant::now() + Duration::from_secs(60);
    while observer.is_live() && Instant::now() < deadline {
        observer.drain(Instant::now());
        aterm_uds::deadline::poll_yield();
    }
    assert!(!observer.is_live());
    assert_eq!(observer.running(), ObserverMode::Off, "the end clears it");
    assert_eq!(
        observer.requested(),
        ObserverMode::Full,
        "an end by itself keeps the request"
    );

    assert_eq!(
        observer.start(ObserverMode::Sentinel, || {}),
        StartOutcome::Started
    );
    observer.feed_for_test(ObserverEvent::Started, now);
    assert_eq!(observer.running(), ObserverMode::Sentinel);
    observer.stop();
    assert_eq!(observer.running(), ObserverMode::Off, "a stop clears it");
    assert_eq!(observer.requested(), ObserverMode::Off);
}

#[test]
fn the_instance_owns_its_process_attribution_probes() {
    fn parent(pid: i32) -> Option<i32> {
        match pid {
            18585 => Some(4711),
            _ => None,
        }
    }
    fn group(pid: i32) -> Option<i32> {
        match pid {
            18584 => Some(4711),
            _ => None,
        }
    }
    let now = Instant::now();
    let later = now + PENDING_PROMPT_THRESHOLD + Duration::from_secs(1);
    let sessions = &[(4_u64, 4711_i32)];
    let mut observer = ObserverState::inert();
    observer.feed_for_test(ObserverEvent::Started, now);
    for line in [REC_CTX, REC_ATTRIBUTION, REC_SUBJECT] {
        observer.feed_for_test(
            ObserverEvent::Line(parse_line(line).expect("fixture line")),
            now,
        );
    }
    assert_eq!(observer.session_for_pid(18584, sessions), None);
    assert_eq!(observer.session_for_pid(18585, sessions), None);
    assert!(
        observer
            .take_new_verdicts_for_instance(later, 1, || sessions.to_vec())
            .is_empty()
    );
    observer.probe.parent_of = parent;
    observer.probe.pgid_of = group;
    assert!(!observer.probe_is_live(), "the stream stays inert");
    assert_eq!(observer.session_for_pid(18584, sessions), Some(4), "group");
    assert_eq!(
        observer.session_for_pid(18585, sessions),
        Some(4),
        "ancestor"
    );
    assert_eq!(observer.session_for_pid(9999, sessions), None, "unrelated");
    let fresh = observer.take_new_verdicts_for_instance(later, 2, || sessions.to_vec());
    assert_eq!(fresh, [4]);
}

#[test]
fn one_observed_prompt_is_announced_once_and_a_second_dialog_again() {
    let t0 = Instant::now();
    let mut observer = ObserverState::inert();
    observer.feed_for_test(ObserverEvent::Started, t0);
    for line in [REC_CTX, REC_ATTRIBUTION, REC_SUBJECT] {
        let event = parse_line(line).expect("recorded line parses");
        observer.feed_for_test(ObserverEvent::Line(event), t0);
    }
    let later = t0 + PENDING_PROMPT_THRESHOLD + Duration::from_secs(1);
    let sessions = || vec![(4u64, 4711i32)];
    let pgid = |pid: i32| (pid == 18584).then_some(4711);

    let first = observer.take_new_verdicts(later, 1, sessions, pgid);
    assert_eq!(first, [4], "the observed dialog is announced");

    assert!(
        observer
            .take_new_verdicts(later, 2, sessions, pgid)
            .is_empty(),
        "the same dialog is never announced twice"
    );

    for line in [
        "tccd: AUTHREQ_CTX: msgID=378.999, service=kTCCServiceSystemPolicyDesktopFolder, \
         preflight=no",
        "tccd: AUTHREQ_ATTRIBUTION: msgID=378.999, attribution={accessing={TCCDProcess: \
         identifier=x, pid=18584}}",
    ] {
        let event = parse_line(line).expect("line parses");
        observer.feed_for_test(ObserverEvent::Line(event), later);
    }
    let later2 = later + PENDING_PROMPT_THRESHOLD + Duration::from_secs(1);
    let second = observer.take_new_verdicts(later2, 2, sessions, pgid);
    assert_eq!(second, [4], "a distinct dialog is distinct news");
}

#[test]
fn a_stream_that_ends_leaves_no_prompt_marking_its_tab() {
    type End = fn(&mut ObserverState);
    fn silent_worker(_: &StreamSink, _: ObserverMode) {}
    let ends: [(&str, End); 4] = [
        ("a stop", |observer| observer.stop()),
        ("the worker's end", |observer| {
            observer.feed_for_test(ObserverEvent::Ended, Instant::now());
        }),
        ("the worker's unavailable", |observer| {
            observer.feed_for_test(
                ObserverEvent::Unavailable(UnavailableReason::NoLogAccess),
                Instant::now(),
            );
        }),
        ("the channel's disconnect", |observer| {
            let deadline = Instant::now() + Duration::from_secs(60);
            while observer.running() != ObserverMode::Off && Instant::now() < deadline {
                observer.drain(Instant::now());
                aterm_uds::deadline::poll_yield();
            }
            assert_eq!(observer.running(), ObserverMode::Off, "the worker returned");
        }),
    ];
    for (end, run) in ends {
        let t0 = Instant::now();
        let mut observer = ObserverState::inert();
        observer.set_arm_for_test(silent_worker);
        assert_eq!(
            observer.start(ObserverMode::Full, || {}),
            StartOutcome::Started
        );
        observer.feed_for_test(ObserverEvent::Started, t0);
        for line in [REC_CTX, REC_ATTRIBUTION] {
            let event = parse_line(line).expect("recorded line parses");
            observer.feed_for_test(ObserverEvent::Line(event), t0);
        }
        let later = t0 + PENDING_PROMPT_THRESHOLD + Duration::from_secs(1);
        let first = observer.take_new_verdicts(
            later,
            1,
            || vec![(4, 4711)],
            |pid| (pid == 18584).then_some(4711),
        );
        assert_eq!(first, [4], "{end}");
        assert!(observer.marks_session(4), "{end}");
        assert!(observer.take_attention_changed(), "{end}");
        run(&mut observer);
        assert!(
            !observer.marks_session(4),
            "after {end} no stream is left to answer it"
        );
        assert!(
            observer.take_attention_changed(),
            "after {end} the host hears that the mark moved"
        );
    }
}

#[test]
fn a_full_ring_of_announced_prompts_evicts_only_an_answered_one() {
    let t0 = Instant::now();
    let mut observer = ObserverState::inert();
    observer.feed_for_test(ObserverEvent::Started, t0);
    let shells: Vec<(u64, i32)> = (0u64..).zip(5000i32..).take(ANNOUNCED_CAP + 1).collect();
    for (session, shell) in &shells {
        for line in [
            format!(
                "tccd: AUTHREQ_CTX: msgID=5.{session}, service=kTCCServiceCamera, preflight=no"
            ),
            format!(
                "tccd: AUTHREQ_ATTRIBUTION: msgID=5.{session}, attribution={{accessing=\
                 {{TCCDProcess: identifier=x, pid={shell}}}}}"
            ),
        ] {
            let event = parse_line(&line).expect("line parses");
            observer.feed_for_test(ObserverEvent::Line(event), t0);
        }
    }
    let later = t0 + PENDING_PROMPT_THRESHOLD + Duration::from_secs(1);
    let sessions = || shells.clone();
    let last = shells[ANNOUNCED_CAP].0;

    let first = observer.take_new_verdicts(later, 1, sessions, |_| None);
    assert_eq!(first.len(), ANNOUNCED_CAP, "the ring takes what it holds");
    assert!(!first.contains(&last), "the prompt past it waits");
    assert!(
        (0..last).all(|session| observer.marks_session(session)),
        "and no pending prompt is evicted for it"
    );

    let answered = TccEvent::Result {
        msg_id: "5.3".to_owned(),
    };
    observer.feed_for_test(ObserverEvent::Line(answered), later);
    assert_eq!(
        observer.take_new_verdicts(later, 1, sessions, |_| None),
        [last],
        "an answered entry makes room"
    );
    assert!(
        (0..=last)
            .filter(|session| *session != 3)
            .all(|session| observer.marks_session(session)),
        "and only it goes"
    );
}

#[test]
fn an_unattributable_prompt_is_never_marked_announced() {
    let t0 = Instant::now();
    let mut observer = ObserverState::inert();
    observer.feed_for_test(ObserverEvent::Started, t0);
    for line in [REC_CTX, REC_ATTRIBUTION] {
        let event = parse_line(line).expect("recorded line parses");
        observer.feed_for_test(ObserverEvent::Line(event), t0);
    }
    let later = t0 + PENDING_PROMPT_THRESHOLD + Duration::from_secs(1);
    assert!(
        observer
            .take_new_verdicts(later, 1, || vec![(4, 9999)], |_| None)
            .is_empty(),
        "not ours"
    );
    let ours = observer.take_new_verdicts(
        later,
        2,
        || vec![(4, 4711)],
        |pid| (pid == 18584).then_some(4711),
    );
    assert_eq!(ours.len(), 1, "and once it IS ours, it is announced");
}

#[test]
fn an_unattributable_prompt_is_judged_once_per_roster() {
    let t0 = Instant::now();
    let mut observer = ObserverState::inert();
    observer.feed_for_test(ObserverEvent::Started, t0);
    for line in [REC_CTX, REC_ATTRIBUTION] {
        let event = parse_line(line).expect("recorded line parses");
        observer.feed_for_test(ObserverEvent::Line(event), t0);
    }
    let later = t0 + PENDING_PROMPT_THRESHOLD + Duration::from_secs(1);
    let asked = std::cell::Cell::new(0usize);
    let pgid = |_pid: i32| {
        asked.set(asked.get() + 1);
        None
    };
    let roster = || vec![(4u64, 9999i32)];
    for park in 0..3 {
        assert!(
            observer
                .take_new_verdicts(later + Duration::from_secs(park), 1, roster, pgid)
                .is_empty()
        );
    }
    assert_eq!(asked.get(), 1, "three parks over one roster judge once");
    assert!(
        observer
            .take_new_verdicts(later, 2, roster, pgid)
            .is_empty()
    );
    assert_eq!(asked.get(), 2, "a moved roster judges it again");
    let event = parse_line(
        "tccd: AUTHREQ_ATTRIBUTION: msgID=378.385, attribution={requesting={TCCDProcess: \
         identifier=x, pid=4242}}",
    )
    .expect("line parses");
    observer.feed_for_test(ObserverEvent::Line(event), later);
    assert!(
        observer
            .take_new_verdicts(later, 2, roster, pgid)
            .is_empty()
    );
    assert_eq!(asked.get(), 3, "a folded line judges it again");
}

#[test]
fn the_threshold_crossing_is_judged_at_its_deadline() {
    let opened_before = Instant::now();
    let t0 = opened_before + PENDING_PROMPT_THRESHOLD + Duration::from_secs(1);
    let mut observer = ObserverState::inert();
    observer.feed_for_test(ObserverEvent::Started, opened_before);
    for line in [
        "tccd: AUTHREQ_CTX: msgID=378.400, service=kTCCServiceSystemPolicyDesktopFolder, \
         preflight=no",
        "tccd: AUTHREQ_ATTRIBUTION: msgID=378.400, attribution={accessing={TCCDProcess: \
         identifier=x, pid=5150}}",
    ] {
        let event = parse_line(line).expect("line parses");
        observer.feed_for_test(ObserverEvent::Line(event), opened_before);
    }
    for line in [REC_CTX, REC_ATTRIBUTION] {
        let event = parse_line(line).expect("recorded line parses");
        observer.feed_for_test(ObserverEvent::Line(event), t0);
    }
    let asked = std::cell::Cell::new(0usize);
    let pgid = |pid: i32| {
        asked.set(asked.get() + 1);
        (pid == 18584).then_some(4711)
    };
    let sessions = || vec![(4u64, 4711i32)];
    assert!(observer.take_new_verdicts(t0, 1, sessions, pgid).is_empty());
    assert_eq!(asked.get(), 1, "the pass judges the request nobody owns");
    let crossing = t0 + PENDING_PROMPT_THRESHOLD;
    assert_eq!(observer.verdict_deadline(), Some(crossing));
    assert!(
        observer
            .take_new_verdicts(crossing - Duration::from_millis(1), 1, sessions, pgid)
            .is_empty(),
        "nothing is judged before the crossing"
    );
    assert_eq!(asked.get(), 1, "and the OS is not asked");
    let fresh = observer.take_new_verdicts(crossing, 1, sessions, pgid);
    assert_eq!(asked.get(), 3, "the crossing judges both requests");
    assert_eq!(fresh, [4], "the crossing announces on its own");
    assert_eq!(observer.verdict_deadline(), None, "nothing left to cross");
}

#[test]
fn pokes_coalesce_until_the_queue_is_drained() {
    let pokes = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&pokes);
    let mut observer = ObserverState::inert();
    let (sink, rx) = observer.channel(move || {
        counter.fetch_add(1, Ordering::Relaxed);
    });
    observer.rx = Some(rx);
    for _ in 0..5 {
        sink.post(ObserverEvent::Started);
    }
    assert_eq!(pokes.load(Ordering::Relaxed), 1, "a burst is one wake");
    assert_eq!(observer.drain(Instant::now()), 5, "every message is kept");
    sink.post(ObserverEvent::Started);
    assert_eq!(
        pokes.load(Ordering::Relaxed),
        2,
        "a post after the drain wakes again"
    );
    assert_eq!(observer.drain(Instant::now()), 1);
}

#[test]
fn an_unavailable_observer_announces_nothing() {
    let t0 = Instant::now();
    let mut observer = ObserverState::inert();
    observer.feed_for_test(ObserverEvent::Started, t0);
    for line in [REC_CTX, REC_ATTRIBUTION] {
        let event = parse_line(line).expect("recorded line parses");
        observer.feed_for_test(ObserverEvent::Line(event), t0);
    }
    observer.feed_for_test(
        ObserverEvent::Unavailable(UnavailableReason::NoLogAccess),
        t0,
    );
    let later = t0 + PENDING_PROMPT_THRESHOLD + Duration::from_secs(1);
    assert!(
        observer
            .take_new_verdicts(
                later,
                1,
                || vec![(4, 4711)],
                |pid| (pid == 18584).then_some(4711)
            )
            .is_empty()
    );
}

#[test]
fn the_stream_header_lines_are_not_messages_and_never_count_as_drift() {
    let filtering = "Filtering the log data using \"process == \"tccd\" AND \
        composedMessage CONTAINS \"AUTHREQ_PROMPTING\"\"";
    let columns = "Timestamp                       Thread     Type        Activity             \
        PID    TTL  ";
    assert_eq!(parse_line(filtering), None);
    assert_eq!(parse_line(columns), None);
    assert_eq!(parse_line("x CONTAINS \"AUTHREQ_PROMPTING\"\""), None);
}

#[test]
fn an_understood_line_lifts_a_format_drift() {
    let now = Instant::now();
    let mut observer = ObserverState::inert();
    observer.feed_for_test(ObserverEvent::Started, now);
    for _ in 0..FORMAT_DRIFT_LIMIT {
        observer.feed_for_test(ObserverEvent::Line(TccEvent::Unparsed), now);
    }
    assert_eq!(
        observer.availability(),
        ObserverAvailability::Unavailable(UnavailableReason::FormatUnrecognized)
    );
    observer.feed_for_test(
        ObserverEvent::Line(TccEvent::Result {
            msg_id: "1.1".to_owned(),
        }),
        now,
    );
    assert_eq!(observer.availability(), ObserverAvailability::Ok);
}

#[test]
fn a_dropped_worker_ends_the_stream_honestly() {
    let now = Instant::now();
    let mut observer = ObserverState::inert();
    observer.feed_for_test(ObserverEvent::Started, now);
    observer.feed_for_test(ObserverEvent::Ended, now);
    assert_eq!(
        observer.availability(),
        ObserverAvailability::Unavailable(UnavailableReason::StreamEnded)
    );
}

#[test]
fn consent_marks_recover_only_denials_the_grant_observed_after() {
    let now = Instant::now();
    let mut gate = AttentionGate::new();
    assert!(gate.mark_session(1, now));
    assert!(gate.mark_session(2, now + Duration::from_secs(2)));
    assert!(gate.recover(now + Duration::from_secs(1)));
    assert!(!gate.marks_session(1));
    assert!(gate.marks_session(2));
    assert!(!gate.mark_session(2, now + Duration::from_secs(4)));
    assert!(!gate.recover(now + Duration::from_secs(3)));
    assert!(gate.marks_session(2), "the repeated refusal is newer");
    assert!(gate.recover(now + Duration::from_secs(4)));
    assert!(!gate.has_denials(), "an equal-time observation suffices");
}

#[test]
fn an_eperm_storm_refreshes_the_status_item_once() {
    let mut gate = AttentionGate::new();
    let first = gate.note(AttentionEvent::ProtectedEperm { session: Some(3) });
    assert_eq!(first.raise_tab, Some(3));
    assert!(first.refresh_status_item, "the first denial is news");
    for _ in 0..50 {
        let again = gate.note(AttentionEvent::ProtectedEperm { session: Some(3) });
        assert_eq!(
            again.raise_tab,
            Some(3),
            "the tab mark is per-tab and cheap"
        );
        assert!(!again.refresh_status_item, "and the glance stays as it is");
    }
}

#[test]
fn a_sentinel_instance_marks_no_tab_and_a_full_instance_does() {
    for mode in [ObserverMode::Sentinel, ObserverMode::Off] {
        let mut gate = AttentionGate::new();
        let outcome = gate
            .note(AttentionEvent::ProtectedEperm { session: Some(3) })
            .for_mode(mode);
        assert_eq!(outcome.raise_tab, None, "{mode:?}: no tab mark");
        assert!(
            outcome.refresh_status_item,
            "{mode:?}: the glance refreshes"
        );
    }
    let mut gate = AttentionGate::new();
    let outcome = gate
        .note(AttentionEvent::ProtectedEperm { session: Some(3) })
        .for_mode(ObserverMode::Full);
    assert_eq!(outcome.raise_tab, Some(3));
}

#[test]
fn an_instance_level_eperm_marks_no_tab() {
    let mut gate = AttentionGate::new();
    let outcome = gate.note(AttentionEvent::ProtectedEperm { session: None });
    assert_eq!(outcome.raise_tab, None);
    assert!(outcome.refresh_status_item, "the first denial is news");
    assert!(gate.has_denials(), "and it waits for the same grant");
}

const REC_PROMPTING_WINDOW: &str = "2026-09-28 17:28:00.090411-0700 0x20dffbe  Default     \
    0x7265e15            3501   0    tccd: [com.apple.TCC:access] AUTHREQ_PROMPTING: \
    msgID=69745.137, service=kTCCServiceSystemPolicyAppData, \
    subject=Sub:{com.aterm.aterm}Resp:{TCCDProcess: identifier=com.aterm.aterm, \
    pid=70374, auid=501, euid=501, \
    responsible_path=/Applications/aterm.app/Contents/MacOS/aterm, \
    binary_path=/Applications/aterm.app/Contents/MacOS/aterm},";
const REC_PROMPTING_ADOPTED: &str = "2026-09-28 17:55:12.310902-0700 0x20dffbe  Default     \
    0x7265e15            3501   0    tccd: [com.apple.TCC:access] AUTHREQ_PROMPTING: \
    msgID=69745.190, service=kTCCServiceSystemPolicyAppData, \
    subject=Sub:{com.aterm.aterm}Resp:{TCCDProcess: identifier=com.aterm.aterm, \
    pid=74756, auid=501, euid=501, \
    responsible_path=/Applications/aterm.app/Contents/MacOS/aterm, \
    binary_path=/Users//someone/Library/Application Support/aterm/pkg/store/claude/1/bin/claude},";
const REC_PROMPTING_ADOPTED_2: &str = "2026-09-28 18:26:50.661120-0700 0x20dffbe  Default     \
    0x7265e15            3501   0    tccd: [com.apple.TCC:access] AUTHREQ_PROMPTING: \
    msgID=69745.222, service=kTCCServiceSystemPolicyAppData, \
    subject=Sub:{com.aterm.aterm}Resp:{TCCDProcess: identifier=com.aterm.aterm, \
    pid=99705, auid=501, euid=501, \
    responsible_path=/Applications/aterm.app/Contents/MacOS/aterm, \
    binary_path=/Users//someone/Library/Application Support/aterm/pkg/store/claude/1/bin/claude},";

const REC_PROMPTING_OTHER_SUBJECT: &str = "2026-09-28 17:31:00.000000-0700 0x1  Default     \
    0x1            3501   0    tccd: [com.apple.TCC:access] AUTHREQ_PROMPTING: \
    msgID=69745.300, service=kTCCServiceSystemPolicyAppData, \
    subject=Sub:{com.example.other}Resp:{TCCDProcess: identifier=com.example.other, \
    pid=1234, auid=501, euid=501, responsible_path=/Applications/Other.app/Contents/MacOS/Other, \
    binary_path=/Applications/Other.app/Contents/MacOS/Other},";

fn prompting(msg_id: &str, subject: &str, resp_pid: i32) -> ObserverEvent {
    ObserverEvent::Prompt(PromptNeed {
        msg_id: msg_id.to_owned(),
        service: "kTCCServiceSystemPolicyAppData".to_owned(),
        subject: subject.to_owned(),
        resp_pid,
        accessing_pid: Some(resp_pid),
    })
}

#[test]
fn the_recorded_prompting_lines_parse_into_prompting_events() {
    for (line, msg_id, pid) in [
        (REC_PROMPTING_WINDOW, "69745.137", 70374),
        (REC_PROMPTING_ADOPTED, "69745.190", 74756),
        (REC_PROMPTING_ADOPTED_2, "69745.222", 99705),
    ] {
        assert_eq!(
            parse_line(line),
            Some(TccEvent::Prompting {
                msg_id: msg_id.to_owned(),
                service: "kTCCServiceSystemPolicyAppData".to_owned(),
                subject: "com.aterm.aterm".to_owned(),
                resp_pid: pid,
            }),
            "{line}"
        );
    }
    assert_eq!(
        parse_line(REC_PROMPTING_OTHER_SUBJECT),
        Some(TccEvent::Prompting {
            msg_id: "69745.300".to_owned(),
            service: "kTCCServiceSystemPolicyAppData".to_owned(),
            subject: "com.example.other".to_owned(),
            resp_pid: 1234,
        })
    );
}

#[test]
fn a_prompting_line_missing_its_fields_is_unparsed_never_guessed() {
    for broken in [
        "tccd: AUTHREQ_PROMPTING: msgID=1.2, service=kTCCServiceSystemPolicyAppData, \
         subject=Sub:{com.aterm.aterm}",
        "tccd: AUTHREQ_PROMPTING: msgID=1.2, service=kTCCServiceSystemPolicyAppData, \
         subject=Sub:{}Resp:{TCCDProcess: identifier=x, pid=5},",
        "tccd: AUTHREQ_PROMPTING: msgID=1.2, subject=Sub:{com.aterm.aterm}Resp:{TCCDProcess: \
         identifier=x, pid=5},",
        "tccd: AUTHREQ_PROMPTING: service=kTCCServiceSystemPolicyAppData, \
         subject=Sub:{com.aterm.aterm}Resp:{TCCDProcess: identifier=x, pid=5},",
        "tccd: AUTHREQ_PROMPTING: msgID=1.2, service=kTCCServiceSystemPolicyAppData, \
         subject=Sub:{com.aterm.aterm}Resp:{TCCDProcess: identifier=x, pid=abc},",
    ] {
        assert_eq!(parse_line(broken), Some(TccEvent::Unparsed), "{broken}");
    }
}

#[test]
fn the_sentinel_predicate_is_narrow_and_names_tccd() {
    assert!(SENTINEL_PREDICATE.starts_with("process == \"tccd\""));
    assert!(SENTINEL_PREDICATE.contains("AUTHREQ_PROMPTING"));
    assert!(
        !SENTINEL_PREDICATE.contains("AUTHREQ_CTX")
            && !SENTINEL_PREDICATE.contains("AUTHREQ_ATTRIBUTION"),
        "the sentinel reads one line, not the correlation stream"
    );
    let sentinel = stream_args(ObserverMode::Sentinel);
    assert_eq!(sentinel[0], "stream");
    let at = sentinel.iter().position(|a| a == "--predicate").unwrap();
    assert_eq!(sentinel[at + 1], SENTINEL_PREDICATE);
    let full = stream_args(ObserverMode::Full);
    let at = full.iter().position(|a| a == "--predicate").unwrap();
    assert_eq!(full[at + 1], "process == \"tccd\"");
    assert!(
        !full.iter().any(|a| a == "--timeout"),
        "full mode is unbounded"
    );
}

#[test]
fn the_sentinel_stream_is_bounded_by_a_timeout_and_rotated_on_eof() {
    let args = stream_args(ObserverMode::Sentinel);
    let at = args.iter().position(|a| a == "--timeout").unwrap();
    assert_eq!(args[at + 1], SENTINEL_TIMEOUT);
    let mins: u64 = SENTINEL_TIMEOUT
        .strip_suffix('m')
        .expect("minute timeout suffix")
        .parse()
        .expect("positive integer minutes");
    let timeout = Duration::from_secs(mins * 60);
    assert!(timeout > Duration::ZERO);
    assert!(ROTATION_MIN < timeout);
    assert!(timeout <= ROTATION_MIN + Duration::from_secs(60));
    assert!(LOOKUP_TIMEOUT <= Duration::from_secs(5));
    assert!(lookup_args("69745.190").is_some());
    for hostile in ["", "1.2\" OR 1", "1.2,", "abc", "1.2 3"] {
        assert!(lookup_args(hostile).is_none(), "{hostile:?}");
    }
    let lookup = lookup_args("69745.190").unwrap();
    assert_eq!(lookup[0], "show");
    assert!(
        lookup
            .iter()
            .any(|a| a.contains("AUTHREQ_ATTRIBUTION: msgID=69745.190,"))
    );
}

#[test]
fn an_end_at_the_timeout_is_a_rotation_not_an_unavailable_observer() {
    let mut policy = RotationPolicy::new();
    let t0 = Instant::now();
    for i in 0..20u64 {
        assert_eq!(
            policy.on_end(
                ObserverMode::Sentinel,
                Duration::from_secs(15 * 60),
                t0 + Duration::from_secs(i * 900)
            ),
            StreamEnd::Rotate
        );
    }
    assert_eq!(
        policy.on_end(ObserverMode::Sentinel, ROTATION_MIN, t0),
        StreamEnd::Rotate,
        "14 min 59 s is the timeout"
    );
    assert_eq!(
        policy.on_end(ObserverMode::Full, Duration::from_secs(20 * 60), t0),
        StreamEnd::GiveUp
    );
    let mut observer = ObserverState::inert();
    let now = Instant::now();
    observer.feed_for_test(ObserverEvent::Started, now);
    assert_eq!(observer.availability(), ObserverAvailability::Ok);
}

#[test]
fn an_early_end_restarts_at_most_three_times_an_hour() {
    let mut policy = RotationPolicy::new();
    let t0 = Instant::now();
    let early = Duration::from_secs(30);
    for i in 0..3u64 {
        assert_eq!(
            policy.on_end(
                ObserverMode::Sentinel,
                early,
                t0 + Duration::from_secs(i * 60)
            ),
            StreamEnd::Restart,
            "early end {i}"
        );
    }
    assert_eq!(
        policy.on_end(ObserverMode::Sentinel, early, t0 + Duration::from_secs(200)),
        StreamEnd::GiveUp,
        "a fourth early end inside the hour gives up"
    );
    assert_eq!(
        policy.on_end(
            ObserverMode::Sentinel,
            Duration::from_secs(15 * 60),
            t0 + Duration::from_secs(300)
        ),
        StreamEnd::Rotate
    );
    assert_eq!(
        policy.on_end(
            ObserverMode::Sentinel,
            early,
            t0 + Duration::from_secs(60 * 60 + 61)
        ),
        StreamEnd::Restart,
        "an hour later the first early ends have aged out"
    );
}

#[test]
fn a_stop_signals_the_child_only_while_it_is_unreaped() {
    let sent = std::cell::RefCell::new(Vec::new());
    let kill = |pid: i32, signal: Signal| sent.borrow_mut().push((pid, signal));

    let standing = ChildSlot::default();
    assert!(standing.publish(4101, kill), "nothing stopped it yet");
    assert!(
        sent.borrow().is_empty(),
        "a publication alone signals nothing"
    );
    standing.stop(kill);
    assert_eq!(
        *sent.borrow(),
        [(4101, Signal::Term)],
        "a standing child is asked to exit, once"
    );

    sent.borrow_mut().clear();
    let reaped = ChildSlot::default();
    assert!(reaped.publish(4102, kill));
    reaped.retire();
    reaped.stop(kill);
    assert!(sent.borrow().is_empty(), "a retired pid is never signalled");
    assert!(reaped.stopped(), "the stop still stands for the worker");

    ChildSlot::default().stop(kill);
    assert!(sent.borrow().is_empty(), "no pid, no signal");
}

#[test]
fn a_stop_before_the_pid_is_published_signals_the_child_at_publication() {
    let sent = std::cell::RefCell::new(Vec::new());
    let kill = |pid: i32, signal: Signal| sent.borrow_mut().push((pid, signal));

    let slot = ChildSlot::default();
    slot.stop(kill);
    assert!(sent.borrow().is_empty());
    assert!(!slot.publish(4201, kill), "the worker reaps it unread");
    assert_eq!(
        *sent.borrow(),
        [(4201, Signal::Term)],
        "signalled exactly once, at its publication"
    );

    sent.borrow_mut().clear();
    assert!(ChildSlot::default().publish(4202, kill));
    assert!(sent.borrow().is_empty());
}

#[test]
fn the_lookup_watchdog_kills_only_a_child_unreaped_at_its_limit() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let slot = Arc::new(ChildSlot::default());
    assert!(slot.publish(4301, |_, _| {}));
    let (done, returned) = std::sync::mpsc::channel();
    let (watched, log) = (Arc::clone(&slot), Arc::clone(&sent));
    std::thread::spawn(move || {
        watched.kill_unless_retired_within(Duration::from_secs(60 * 60), |pid, signal| {
            log.lock().unwrap().push((pid, signal));
        });
        let _ = done.send(());
    });
    slot.retire();
    returned
        .recv_timeout(Duration::from_secs(60))
        .expect("the watchdog returns when its child is retired");
    assert!(
        sent.lock().unwrap().is_empty(),
        "a retired child is never killed"
    );

    let stuck = ChildSlot::default();
    assert!(stuck.publish(4302, |_, _| {}));
    stuck.kill_unless_retired_within(Duration::ZERO, |pid, signal| {
        sent.lock().unwrap().push((pid, signal));
    });
    assert_eq!(*sent.lock().unwrap(), [(4302, Signal::Kill)]);
}

#[test]
fn a_stop_ends_the_restart_pause() {
    let slot = Arc::new(ChildSlot::default());
    assert!(!slot.pause_unless_stopped(Duration::ZERO));
    let (done, returned) = std::sync::mpsc::channel();
    let paused = Arc::clone(&slot);
    std::thread::spawn(move || {
        let _ = done.send(paused.pause_unless_stopped(Duration::from_secs(60 * 60)));
    });
    slot.stop(|_, _| {});
    assert_eq!(
        returned.recv_timeout(Duration::from_secs(60)),
        Ok(true),
        "the stop ends the pause, and the worker reads it as a stop"
    );
}

#[test]
fn one_msg_id_is_one_need_however_often_the_line_repeats() {
    let mut observer = ObserverState::inert();
    let now = Instant::now();
    for _ in 0..5 {
        observer.feed_for_test(prompting("69745.190", "com.aterm.aterm", 74756), now);
    }
    observer.feed_for_test(prompting("69745.222", "com.aterm.aterm", 99705), now);
    observer.feed_for_test(prompting("69745.300", "com.example.other", 1234), now);
    assert!(
        observer.take_prompting(None).is_empty(),
        "an unknown identity takes nothing"
    );
    let taken = observer.take_prompting(Some("com.aterm.aterm"));
    assert_eq!(
        taken.iter().map(|n| n.msg_id.as_str()).collect::<Vec<_>>(),
        ["69745.190", "69745.222"],
        "each dialog once, another app's not at all"
    );
    assert!(observer.take_prompting(Some("com.aterm.aterm")).is_empty());
    observer.feed_for_test(prompting("69745.190", "com.aterm.aterm", 74756), now);
    assert!(observer.take_prompting(Some("com.aterm.aterm")).is_empty());
    observer.feed_for_test(prompting("69745.400", "com.aterm.aterm.dev", 7), now);
    assert!(observer.take_prompting(Some("com.aterm.aterm")).is_empty());
}

#[test]
fn a_dialog_is_understood_so_it_does_not_look_like_a_format_change() {
    let mut observer = ObserverState::inert();
    let now = Instant::now();
    observer.feed_for_test(ObserverEvent::Started, now);
    observer.feed_for_test(prompting("1.1", "com.aterm.aterm", 5), now);
    for _ in 0..(FORMAT_DRIFT_LIMIT + 1) {
        observer.feed_for_test(ObserverEvent::Line(TccEvent::Unparsed), now);
    }
    assert_eq!(
        observer.availability(),
        ObserverAvailability::Ok,
        "one understood dialog keeps drift from reading as a format change"
    );
}

#[test]
fn the_lookup_reads_the_accessing_pid_from_the_attribution_line() {
    let out = "\
2026-09-28 17:27:59.877879-0700 0x20e0417  Default     0x6a638eb            69689  0    tccd: \
[com.apple.TCC:access] AUTHREQ_ATTRIBUTION: msgID=69745.190, attribution={responsible=\
{TCCDProcess: identifier=com.aterm.aterm, pid=70374, auid=501, euid=501, \
responsible_path=/Applications/aterm.app/Contents/MacOS/aterm, \
binary_path=/Applications/aterm.app/Contents/MacOS/aterm}, accessing={TCCDProcess: \
identifier=com.example.tool, pid=44141, auid=501, euid=501, \
binary_path=/Users//someone/bin/tool}, requesting={TCCDProcess: identifier=\
com.apple.sandboxd, pid=69745, auid=0, euid=0, binary_path=/usr/libexec/sandboxd}, },
";
    assert_eq!(accessing_pid_in(out, "69745.190"), Some(44141));
    assert_eq!(
        accessing_pid_in(out, "69745.191"),
        None,
        "another dialog's line"
    );
    assert_eq!(accessing_pid_in("", "69745.190"), None);
    assert_eq!(accessing_pid_in("not a log line", "69745.190"), None);
}

#[test]
fn a_program_is_placed_in_its_tab_by_ancestry() {
    let sessions = [(1_u64, 100), (2, 200)];
    let parent = |pid: i32| match pid {
        500 => Some(400),
        400 => Some(300),
        300 => Some(200),
        200 => Some(2),
        _ => None,
    };
    let no_group = |_pid: i32| None;
    assert_eq!(session_of_pid(500, &sessions, parent, no_group), Some(2));
    assert_eq!(session_of_pid(200, &sessions, parent, no_group), Some(2));
    assert_eq!(session_of_pid(999, &sessions, parent, no_group), None);
    assert_eq!(session_of_pid(0, &sessions, parent, no_group), None);
    let grouped = |pid: i32| (pid == 300).then_some(100);
    assert_eq!(session_of_pid(500, &sessions, parent, grouped), Some(1));
    let cyclic = |pid: i32| Some(if pid == 7 { 8 } else { 7 });
    assert_eq!(session_of_pid(7, &sessions, cyclic, no_group), None);
}
