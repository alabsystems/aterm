// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    AccessRecord, CardFacts, CardPhase, CardState, DECIDE_ATTEMPTS, DECIDE_TIMEOUT, LAUNCH_SETTLE,
    LaunchVerdict, MARKER, MAX_MARKER_BYTES, MAX_RECORD_BYTES, Marker, NeedCause, NeedFacts,
    NeedVerdict, NeedWords, NotDue, OWNER_ACTED_HOLD, RECORD, RENEW_AFTER, TabPlace, Verdict,
    WATCH_FOR, clear_opened, decide_launch, decide_need, is_prompt_while_granted, marker_path,
    read_marker, read_record, record_marker, record_path, report_state, retire_not_now,
    should_reassert, write_record, write_verified_host,
};
use aterm_containment::consent::{DrClass, FdaState, Folder};

#[test]
fn the_verified_stamp_keeps_every_mac_that_verified() {
    use super::{MAX_VERIFIED_HOSTS, verified_here, with_verified_host, write_verified_host};
    let a = "AAAAAAAA-0000-0000-0000-000000000000";
    let b = "BBBBBBBB-0000-0000-0000-000000000000";
    let once = with_verified_host("", a).expect("a new Mac is added");
    assert_eq!(once, format!("{a}\n"));
    assert_eq!(with_verified_host(&once, a), None, "never twice");
    let both = with_verified_host(&once, b).expect("a second Mac is added");
    assert_eq!(both, format!("{a}\n{b}\n"));
    let mut many = String::new();
    for n in 0..=MAX_VERIFIED_HOSTS {
        many = with_verified_host(&many, &format!("{n:08}-0000-0000-0000-000000000000"))
            .expect("added");
    }
    assert_eq!(many.lines().count(), MAX_VERIFIED_HOSTS);
    assert!(!many.contains("00000000-0000"), "the oldest left first");

    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("aterm.toml");
    let release = Some("com.aterm.aterm");
    let dev = Some("com.aterm.aterm.dev");
    assert_eq!(
        verified_here(Some(&config), Some(a), release),
        None,
        "no stamp"
    );
    write_verified_host(Some(&config), a, "com.aterm.aterm").expect("stamp");
    write_verified_host(Some(&config), b, "com.aterm.aterm").expect("stamp");
    assert_eq!(verified_here(Some(&config), Some(a), release), Some(true));
    assert_eq!(verified_here(Some(&config), Some(b), release), Some(true));
    assert_eq!(
        verified_here(Some(&config), Some(a), dev),
        Some(false),
        "another bundle's grant on this Mac is not this one's"
    );
    assert_eq!(
        verified_here(
            Some(&config),
            Some("CCCCCCCC-0000-0000-0000-000000000000"),
            release
        ),
        Some(false)
    );
    assert_eq!(verified_here(Some(&config), None, release), None, "no UUID");
    assert_eq!(
        verified_here(Some(&config), Some(a), None),
        None,
        "no bundle id"
    );
}
use std::time::{Duration, Instant};

fn denied_release() -> CardFacts {
    CardFacts {
        offered: true,
        headless: false,
        fda: FdaState::Denied,
        dr: DrClass::Identity,
    }
}

fn bounded_wait_model() -> aterm_spec::derive::Model {
    aterm_spec::ty_model! {
        MacosAccessCardBoundedWait {
            const Buggy = 0;
            var phase = 2;
            var expired = 0;
            action Raise when (phase == 2 && expired == 0) {
                phase = 3;
            }
            action Expire when (phase > 1 && phase <= 3 && expired == 0) {
                expired = 1;
                phase = if Buggy == 1 && phase == 2 { phase } else { 4 };
            }
            invariant Bounds: phase > 1 && phase <= 4 && expired <= 1;
            invariant NoWorkPastCap: expired == 0 || phase == 4;
        }
    }
}

#[test]
fn bounded_wait_model_proves_and_catches_a_never_shown_card() {
    let model = bounded_wait_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
}

#[test]
fn bounded_wait_conforms_at_each_real_phase_deadline() {
    let now = Instant::now();
    let model = bounded_wait_model();
    let scenarios: &[&[&str]] = &[&[], &["Raise"]];
    let mut negative_controls = 0;
    for actions in scenarios {
        let mut actual = CardState::new();
        assert_eq!(actual.lifecycle_deadline(), None);
        assert!(actual.begin_deciding(now));
        assert_eq!(actual.lifecycle_deadline(), Some(now + DECIDE_TIMEOUT));
        assert!(actual.on_decided(&Verdict::Offer, now));
        assert_eq!(
            actual.lifecycle_deadline(),
            Some(now + WATCH_FOR),
            "a due card waits on a pending probe with the watch's patience"
        );
        let mut expected = model.init_state();
        for (step, action) in actions.iter().enumerate() {
            let at = now + Duration::from_secs(step as u64 + 1);
            match *action {
                "Raise" => actual.on_raised(at),
                _ => unreachable!(),
            }
            assert!(model.fire(action, &mut expected));
            let phase = match actual.phase() {
                CardPhase::Due => 2,
                CardPhase::Watching { .. } => 3,
                _ => panic!("unexpected waiting phase"),
            };
            assert_eq!(phase, expected["phase"]);
        }
        let deadline = actual.lifecycle_deadline().expect("every wait has a cap");
        assert!(!actual.expire_wait(deadline - Duration::from_nanos(1)));
        assert_eq!(actual.lifecycle_deadline(), Some(deadline));
        let before = expected.clone();
        assert!(actual.expire_wait(deadline));
        assert!(model.fire("Expire", &mut expected));
        assert_eq!(actual.phase(), CardPhase::Settled);
        assert_eq!(expected["phase"], 4);
        assert!(model.check_invariant("NoWorkPastCap", &expected));
        assert_eq!(
            actual.lifecycle_deadline(),
            None,
            "a settled card owns no deadline: there is no daily return any more"
        );
        assert!(!actual.raise_allowed(deadline));
        assert!(!actual.wants_probe(deadline));
        if before["phase"] == 2 {
            let mut old = expected.clone();
            old.insert("phase", 2);
            assert!(!model.check_invariant("NoWorkPastCap", &old));
            negative_controls += 1;
        }
    }
    assert_eq!(negative_controls, 1, "the first-offer omission");
}

fn current_policy_model() -> aterm_spec::derive::Model {
    aterm_spec::ty_model! {
        MacosAccessCardCurrentPolicy {
            const Buggy = 0;
            var enabled = 1;
            var notice = 1;
            var phase = 0;
            action Begin when (phase == 0 && enabled == 1 && notice == 1) {
                phase = 1;
            }
            action Decide when (phase == 1 && enabled == 1 && notice == 1) {
                phase = 2;
            }
            action Raise when (phase == 2 && enabled == 1 && notice == 1) {
                phase = 3;
            }
            action Finish when (phase == 3) {
                phase = 4;
            }
            action DisableMaster when (enabled == 1) {
                enabled = 0;
                phase = if Buggy == 1 && phase > 0 { phase } else { 4 };
            }
            action DisableNotice when (notice == 1) {
                notice = 0;
                phase = if Buggy == 1 && phase > 0 { phase } else { 4 };
            }
            action EnableBoth when (enabled == 0 || notice == 0) {
                enabled = 1;
                notice = 1;
            }
            action Rearm when (phase == 4 && (Buggy == 1 || (enabled == 1 && notice == 1))) {
                phase = 1;
            }
            invariant Bounds: enabled <= 1 && notice <= 1 && phase <= 4;
            invariant DisabledSettles: enabled == 1 && notice == 1 || phase == 4;
        }
    }
}

#[test]
fn current_policy_model_proves_and_catches_a_stale_card() {
    let model = current_policy_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
}

#[test]
fn current_policy_conforms_in_every_real_phase() {
    fn phase_value(state: CardState) -> i64 {
        match state.phase() {
            CardPhase::Idle => 0,
            CardPhase::Deciding { .. } => 1,
            CardPhase::Due => 2,
            CardPhase::Watching { .. } => 3,
            CardPhase::Settled => 4,
        }
    }

    let now = Instant::now();
    let model = current_policy_model();
    let mut actual = CardState::new();
    let mut expected = model.init_state();
    let mut phases = vec![(actual, expected.clone())];
    for action in ["Begin", "Decide", "Raise", "Finish"] {
        match action {
            "Begin" => assert!(actual.begin_deciding(now)),
            "Decide" => assert!(actual.on_decided(&Verdict::Offer, now)),
            "Raise" => actual.on_raised(now),
            "Finish" => actual.settle(),
            _ => unreachable!(),
        }
        assert!(model.fire(action, &mut expected));
        assert_eq!(phase_value(actual), expected["phase"]);
        phases.push((actual, expected.clone()));
    }
    let mut caught_old_guards = 0;
    for (before, projection) in phases {
        for (enabled, notice) in [(true, true), (false, true), (true, false), (false, false)] {
            let mut actual = before;
            let mut expected = projection.clone();
            let admitted = actual.admit_current_policy(enabled, notice);
            assert_eq!(admitted, enabled && notice);
            if !enabled {
                assert!(model.fire("DisableMaster", &mut expected));
            }
            if !notice {
                assert!(model.fire("DisableNotice", &mut expected));
            }
            assert_eq!(phase_value(actual), expected["phase"]);
            assert!(model.check_invariant("DisabledSettles", &expected));
            if admitted {
                assert_eq!(actual, before, "an enabled policy preserves every phase");
            } else {
                let mut old = expected.clone();
                old.insert("phase", phase_value(before));
                if !matches!(before.phase(), CardPhase::Idle | CardPhase::Settled) {
                    assert!(!model.check_invariant("DisabledSettles", &old));
                    caught_old_guards += 1;
                }
                assert!(!actual.on_decided(&Verdict::Offer, now));
                assert!(!actual.on_decided(&Verdict::Confirm, now));
                assert!(actual.admit_current_policy(true, true));
                assert!(model.fire("EnableBoth", &mut expected));
                assert_eq!(phase_value(actual), expected["phase"]);
                assert_eq!(actual.phase(), CardPhase::Settled);
            }
        }
    }
    assert_eq!(
        caught_old_guards, 9,
        "three active phases, three disabled policies"
    );

    for (enabled, notice) in [(true, true), (false, true), (true, false), (false, false)] {
        let mut actual = CardState::new();
        actual.settle();
        let mut expected = model.init_state();
        for action in ["Begin", "Decide", "Raise", "Finish"] {
            assert!(model.fire(action, &mut expected));
        }
        if !enabled {
            assert!(model.fire("DisableMaster", &mut expected));
        }
        if !notice {
            assert!(model.fire("DisableNotice", &mut expected));
        }
        assert_eq!(expected["phase"], 4);
        let admitted = actual.admit_current_policy(enabled, notice);
        assert_eq!(admitted, model.action_enabled("Rearm", &expected));
        if admitted {
            assert!(actual.on_need(now));
            assert!(model.fire("Rearm", &mut expected));
            assert_eq!(phase_value(actual), expected["phase"]);
        } else {
            assert_eq!(actual.phase(), CardPhase::Settled);
            let mut buggy = expected.clone();
            buggy.insert("phase", 1);
            assert!(!model.check_invariant("DisabledSettles", &buggy));
        }
    }
}

fn need_facts(cause: NeedCause) -> NeedFacts {
    NeedFacts {
        card: denied_release(),
        now_secs: 10_000_000,
        process_started_at: 9_999_000,
        cause,
    }
}

fn app_data_prompt() -> NeedCause {
    NeedCause::PromptObserved {
        service: Some(Folder::AppData),
        session: Some(1),
    }
}

#[test]
fn a_verified_grant_reading_off_in_a_fresh_process_is_settling_not_regressed() {
    let verified = AccessRecord {
        verified_at: Some(9_000_000),
        ..AccessRecord::default()
    };
    let fresh = NeedFacts {
        process_started_at: 10_000_000 - 5,
        ..need_facts(app_data_prompt())
    };
    assert_eq!(
        decide_need(&fresh, None, &verified),
        NeedVerdict::Quiet(NotDue::Settling)
    );
    let settled = NeedFacts {
        process_started_at: 10_000_000 - LAUNCH_SETTLE.as_secs(),
        ..fresh.clone()
    };
    assert_eq!(
        decide_need(&settled, None, &verified),
        NeedVerdict::Ask { regressed: true },
        "the second denied read marks the grant regressed"
    );
    assert_eq!(
        decide_need(&fresh, None, &AccessRecord::default()),
        NeedVerdict::Ask { regressed: false }
    );
}

#[test]
fn a_prompt_while_the_probe_reads_granted_logs_an_anomaly_and_asks_nothing() {
    let granted = NeedFacts {
        card: CardFacts {
            fda: FdaState::Granted,
            ..denied_release()
        },
        ..need_facts(app_data_prompt())
    };
    let verdict = decide_need(&granted, None, &AccessRecord::default());
    assert_eq!(verdict, NeedVerdict::Quiet(NotDue::Granted));
    assert!(is_prompt_while_granted(app_data_prompt(), &verdict));
    assert!(!is_prompt_while_granted(
        NeedCause::ProtectedRead { session: None },
        &verdict
    ));
    assert!(!is_prompt_while_granted(
        app_data_prompt(),
        &NeedVerdict::Ask { regressed: false }
    ));
}

#[test]
fn the_opened_state_lapses_with_the_owner_acted_hold() {
    let record = AccessRecord {
        opened_at: Some(1_000_000),
        ..AccessRecord::default()
    };
    let hold = OWNER_ACTED_HOLD.as_secs();
    assert_eq!(
        report_state(None, &record, FdaState::Denied, 1_000_000 + hold - 1),
        "opened"
    );
    assert_eq!(
        report_state(None, &record, FdaState::Denied, 1_000_000 + hold),
        "quiet"
    );
}

#[test]
fn decide_need_is_a_table_of_named_refusals_then_an_ask() {
    let none = AccessRecord::default();
    let facts = need_facts(app_data_prompt());
    assert_eq!(
        decide_need(&facts, None, &none),
        NeedVerdict::Ask { regressed: false }
    );
    assert_eq!(
        decide_need(&facts, Some(Marker::Opened), &none),
        NeedVerdict::Ask { regressed: false },
        "an opened marker is not an answer"
    );
    let off = NeedFacts {
        card: CardFacts {
            offered: false,
            ..denied_release()
        },
        ..facts.clone()
    };
    assert_eq!(
        decide_need(&off, None, &none),
        NeedVerdict::Quiet(NotDue::Off)
    );
    let headless = NeedFacts {
        card: CardFacts {
            headless: true,
            ..denied_release()
        },
        ..facts.clone()
    };
    assert_eq!(
        decide_need(&headless, None, &none),
        NeedVerdict::Quiet(NotDue::Headless)
    );
    for service in [Some(Folder::Documents), Some(Folder::Desktop), None] {
        let other = need_facts(NeedCause::PromptObserved {
            service,
            session: None,
        });
        assert_eq!(
            decide_need(&other, None, &none),
            NeedVerdict::Quiet(NotDue::NotThisClass),
            "{service:?}"
        );
    }
    let eperm = need_facts(NeedCause::ProtectedRead { session: Some(2) });
    assert_eq!(
        decide_need(&eperm, None, &none),
        NeedVerdict::Ask { regressed: false }
    );
    assert_eq!(
        decide_need(&facts, Some(Marker::NotNow), &none),
        NeedVerdict::Quiet(NotDue::Answered)
    );
    for dr in [DrClass::Cdhash, DrClass::Unsigned, DrClass::Unknown] {
        let unstable = NeedFacts {
            card: CardFacts {
                dr,
                ..denied_release()
            },
            ..facts.clone()
        };
        assert_eq!(
            decide_need(&unstable, None, &none),
            NeedVerdict::Quiet(NotDue::GrantUnstable(dr)),
            "{dr:?} is not grant-stable"
        );
    }
    let granted = NeedFacts {
        card: CardFacts {
            fda: FdaState::Granted,
            ..denied_release()
        },
        ..facts.clone()
    };
    assert_eq!(
        decide_need(&granted, None, &none),
        NeedVerdict::Quiet(NotDue::Granted)
    );
    let not_observed = NeedFacts {
        card: CardFacts {
            fda: FdaState::Unknown,
            ..denied_release()
        },
        ..facts.clone()
    };
    assert_eq!(
        decide_need(&not_observed, None, &none),
        NeedVerdict::Quiet(NotDue::NotObservedDenied),
        "unknown is not denied"
    );
    let asked = AccessRecord {
        asked_at: Some(facts.now_secs - 60),
        ..none
    };
    assert_eq!(
        decide_need(&facts, None, &asked),
        NeedVerdict::Quiet(NotDue::RecentlyAsked)
    );
    let week = RENEW_AFTER.as_secs();
    let inside = NeedFacts {
        now_secs: facts.now_secs - 61 + week,
        ..facts.clone()
    };
    assert_eq!(
        decide_need(&inside, None, &asked),
        NeedVerdict::Quiet(NotDue::RecentlyAsked),
        "one second short of asked_at + the week is still inside it"
    );
    let renewed = NeedFacts {
        now_secs: facts.now_secs - 60 + week,
        ..facts.clone()
    };
    assert_eq!(
        decide_need(&renewed, None, &asked),
        NeedVerdict::Ask { regressed: false },
        "the renewal boundary is exact"
    );
    for reason in [
        NotDue::Off,
        NotDue::Headless,
        NotDue::Answered,
        NotDue::Granted,
        NotDue::NotObservedDenied,
        NotDue::GrantUnstable(DrClass::Cdhash),
        NotDue::NotThisClass,
        NotDue::OwnerActed,
        NotDue::RecentlyAsked,
        NotDue::NoNeedYet,
        NotDue::Held,
        NotDue::ReadsOffAfterVerified,
    ] {
        let line = reason.to_string();
        assert!(!line.is_empty() && !line.contains('\n'), "{line:?}");
    }
}

#[test]
fn an_ask_after_a_verified_grant_reads_regressed_and_opens_a_new_epoch() {
    let facts = need_facts(app_data_prompt());
    let t = facts.now_secs;
    let record = AccessRecord {
        asked_at: Some(t - 3600),
        verified_at: Some(t - 1800),
        verified_build: Some(7),
        ..AccessRecord::default()
    };
    assert_eq!(
        decide_need(&facts, None, &record),
        NeedVerdict::Ask { regressed: true },
        "an ask made before the verification does not bar the new epoch"
    );
    let asked_again = AccessRecord {
        asked_at: Some(t - 10),
        ..record
    };
    assert_eq!(
        decide_need(&facts, None, &asked_again),
        NeedVerdict::Quiet(NotDue::RecentlyAsked)
    );
}

/// A grant read off while settling is looked at once settled, and, still off, once more at the
/// next focus gain; a look that reads it held ends the looks. Negative control: nothing is
/// read before the settled time, and a focus gain before the settled look reads nothing.
#[test]
fn a_grant_read_off_while_settling_is_looked_at_once_settled_then_at_focus() {
    use super::{SettleLook, SettleRead};
    let t0 = Instant::now();
    let mut look = SettleLook::after_launch(Duration::from_secs(1), t0);
    let at = t0 + LAUNCH_SETTLE - Duration::from_secs(1);
    assert_eq!(look.deadline(), Some(at));
    look.focus();
    assert!(!look.start(at - Duration::from_secs(1)), "not before");
    assert_eq!(
        look.read(FdaState::Granted),
        None,
        "the control: no read owed"
    );
    assert!(look.start(at), "settled: read afresh");
    assert!(!look.start(at), "once");
    assert_eq!(look.deadline(), None);
    assert_eq!(
        look.read(FdaState::Denied),
        Some(SettleRead::StillOff { settled: true })
    );
    assert!(!look.reading());
    look.focus();
    assert!(look.reading(), "the focus gain's read");
    assert_eq!(
        look.read(FdaState::Granted),
        Some(SettleRead::Held),
        "reads held now"
    );
    look.focus();
    assert_eq!(look, SettleLook::Idle, "no more looks");

    let mut look = SettleLook::after_launch(LAUNCH_SETTLE * 2, t0);
    assert_eq!(look.deadline(), Some(t0 + Duration::from_secs(1)));
    assert!(look.start(t0 + Duration::from_secs(1)));
    assert_eq!(look.read(FdaState::Granted), Some(SettleRead::Held));
    look.focus();
    assert_eq!(look, SettleLook::Idle, "held: no focus look");

    let mut look = SettleLook::after_launch(Duration::ZERO, t0);
    assert!(look.start(t0 + LAUNCH_SETTLE));
    let _ = look.read(FdaState::Denied);
    look.focus();
    assert_eq!(
        look.read(FdaState::Denied),
        Some(SettleRead::StillOff { settled: false }),
        "still off"
    );
    look.focus();
    assert_eq!(look, SettleLook::Idle, "two looks, no more");
}

#[test]
fn decide_launch_never_asks_and_confirms_only_an_opened_grant() {
    let none = AccessRecord::default();
    let denied = denied_release();
    let settled = Duration::from_secs(120);
    assert_eq!(
        decide_launch(&denied, settled, None, &none),
        LaunchVerdict::Quiet(NotDue::NoNeedYet),
        "a denied probe alone raises nothing"
    );
    assert_eq!(
        decide_launch(&denied, settled, Some(Marker::Opened), &none),
        LaunchVerdict::Quiet(NotDue::NoNeedYet),
        "opened-but-still-denied is not an answer, and not a question either"
    );
    let off = CardFacts {
        offered: false,
        ..denied_release()
    };
    assert_eq!(
        decide_launch(&off, settled, None, &none),
        LaunchVerdict::Quiet(NotDue::Off)
    );
    let headless = CardFacts {
        headless: true,
        ..denied_release()
    };
    assert_eq!(
        decide_launch(&headless, settled, None, &none),
        LaunchVerdict::Quiet(NotDue::Headless)
    );
    assert_eq!(
        decide_launch(&denied, settled, Some(Marker::NotNow), &none),
        LaunchVerdict::Quiet(NotDue::Answered)
    );
    let granted = CardFacts {
        fda: FdaState::Granted,
        ..denied_release()
    };
    assert_eq!(
        decide_launch(&granted, settled, None, &none),
        LaunchVerdict::Quiet(NotDue::Granted)
    );
    assert_eq!(
        decide_launch(&granted, settled, Some(Marker::Opened), &none),
        LaunchVerdict::Confirm,
        "the flip the owner started in an earlier process is acknowledged once"
    );
    let asked = AccessRecord {
        asked_at: Some(5),
        ..none
    };
    assert_eq!(
        decide_launch(&granted, settled, None, &asked),
        LaunchVerdict::Confirm,
        "a grant after an ask is acknowledged whichever gesture made it"
    );
    let verified = AccessRecord {
        asked_at: Some(5),
        verified_at: Some(9),
        ..none
    };
    assert_eq!(
        decide_launch(&granted, settled, None, &verified),
        LaunchVerdict::Quiet(NotDue::Held),
        "already verified: nothing to say again"
    );
    assert_eq!(
        decide_launch(&granted, settled, Some(Marker::NotNow), &none),
        LaunchVerdict::Quiet(NotDue::Answered)
    );
    assert_eq!(
        decide_launch(&denied, settled, None, &verified),
        LaunchVerdict::Quiet(NotDue::ReadsOffAfterVerified),
        "quiet and LOGGED; the next ask's words say it regressed"
    );
    assert_eq!(
        decide_launch(&denied, Duration::from_secs(1), None, &verified),
        LaunchVerdict::Quiet(NotDue::Settling),
        "an update's successor at its first second: read again once settled"
    );
    assert_eq!(
        decide_launch(&denied, Duration::from_secs(1), None, &none),
        LaunchVerdict::Quiet(NotDue::NoNeedYet),
        "nothing verified: nothing to settle"
    );
    assert_eq!(
        decide_launch(&granted, Duration::from_secs(1), None, &verified),
        LaunchVerdict::Quiet(NotDue::Held),
        "held is held at any age"
    );
    let unknown = CardFacts {
        fda: FdaState::Unknown,
        ..denied_release()
    };
    assert_eq!(
        decide_launch(&unknown, settled, None, &none),
        LaunchVerdict::Quiet(NotDue::NoNeedYet)
    );
    assert!(should_reassert(&asked, None, FdaState::Denied));
    assert!(!should_reassert(
        &asked,
        Some(Marker::Opened),
        FdaState::Denied
    ));
    assert!(!should_reassert(
        &asked,
        Some(Marker::NotNow),
        FdaState::Denied
    ));
    assert!(!should_reassert(&asked, None, FdaState::Granted));
    assert!(!should_reassert(&none, None, FdaState::Denied));
    assert!(!should_reassert(&verified, None, FdaState::Denied));
}

#[test]
fn an_owner_who_opened_settings_is_not_asked_again_by_an_older_window() {
    let t = 10_000_000_u64;
    let record = AccessRecord {
        asked_at: Some(t - 100),
        opened_at: Some(t - 50),
        ..AccessRecord::default()
    };
    let older_window = NeedFacts {
        now_secs: t + RENEW_AFTER.as_secs() + 1,
        process_started_at: t - 5_000,
        ..need_facts(app_data_prompt())
    };
    assert_eq!(
        decide_need(&older_window, None, &record),
        NeedVerdict::Quiet(NotDue::OwnerActed),
        "the week elapsed and the probe reads denied, but this window predates the press"
    );
    let lapsed = NeedFacts {
        now_secs: t - 50 + OWNER_ACTED_HOLD.as_secs(),
        ..older_window.clone()
    };
    assert_eq!(
        decide_need(&lapsed, None, &record),
        NeedVerdict::Ask { regressed: false },
        "the hold lapses after fourteen days and the week has long elapsed"
    );
    let just_inside = NeedFacts {
        now_secs: t - 51 + OWNER_ACTED_HOLD.as_secs(),
        ..older_window.clone()
    };
    assert_eq!(
        decide_need(&just_inside, None, &record),
        NeedVerdict::Quiet(NotDue::OwnerActed)
    );
    let newer_window = NeedFacts {
        process_started_at: t,
        ..older_window.clone()
    };
    assert_eq!(
        decide_need(&newer_window, None, &record),
        NeedVerdict::Ask { regressed: false }
    );
    let newer_soon = NeedFacts {
        now_secs: t + 60,
        process_started_at: t,
        ..older_window.clone()
    };
    assert_eq!(
        decide_need(&newer_soon, None, &record),
        NeedVerdict::Quiet(NotDue::RecentlyAsked)
    );
    let verified = AccessRecord {
        verified_at: Some(t),
        ..record
    };
    assert_eq!(
        decide_need(&older_window, None, &verified),
        NeedVerdict::Ask { regressed: true }
    );
}

#[test]
fn the_marker_round_trips_beside_the_config() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-consent-card-{}-{}",
        std::process::id(),
        aterm_time::since_epoch().unwrap().as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let config = dir.join("aterm.toml");
    assert_eq!(marker_path(Some(&config)), Some(dir.join(MARKER)));
    assert_eq!(marker_path(None), None);
    assert_eq!(read_marker(Some(&config)), None);
    assert!(record_marker(None, Marker::NotNow).is_err());
    assert!(
        clear_opened(None).is_ok(),
        "nothing to clear is not an error"
    );

    record_marker(Some(&config), Marker::Opened).unwrap();
    assert_eq!(read_marker(Some(&config)), Some(Marker::Opened));
    clear_opened(Some(&config)).unwrap();
    assert_eq!(read_marker(Some(&config)), None, "opened is consumed");
    assert!(!dir.join(MARKER).exists());

    record_marker(Some(&config), Marker::NotNow).unwrap();
    assert_eq!(read_marker(Some(&config)), Some(Marker::NotNow));
    record_marker(Some(&config), Marker::Opened).unwrap();
    assert_eq!(
        read_marker(Some(&config)),
        Some(Marker::NotNow),
        "opening Settings is not an answer: it never replaces the owner's not-now"
    );
    clear_opened(Some(&config)).unwrap();
    assert_eq!(
        read_marker(Some(&config)),
        Some(Marker::NotNow),
        "an answer is the owner's and is never cleared"
    );
    // System State spent it on an approved revision: retired, and only it.
    retire_not_now(Some(&config)).unwrap();
    assert_eq!(read_marker(Some(&config)), None, "not-now is retired");
    assert!(!dir.join(MARKER).exists());
    record_marker(Some(&config), Marker::Opened).unwrap();
    retire_not_now(Some(&config)).unwrap();
    assert_eq!(
        read_marker(Some(&config)),
        Some(Marker::Opened),
        "an opened is not an answer to retire"
    );
    clear_opened(Some(&config)).unwrap();
    assert!(retire_not_now(Some(&config)).is_ok(), "nothing to retire");
    assert!(retire_not_now(None).is_ok());
    record_marker(Some(&config), Marker::NotNow).unwrap();
    for marker in [Marker::NotNow, Marker::Opened] {
        assert!(marker.as_str().len() <= MAX_MARKER_BYTES);
        assert_eq!(Marker::parse(marker.as_str()), Some(marker));
    }
    std::fs::write(dir.join(MARKER), "not-now\n").unwrap();
    assert_eq!(read_marker(Some(&config)), Some(Marker::NotNow));
    std::fs::write(dir.join(MARKER), "x".repeat(MAX_MARKER_BYTES + 1)).unwrap();
    assert_eq!(read_marker(Some(&config)), None);
    std::fs::write(dir.join(MARKER), "granted").unwrap();
    assert_eq!(
        read_marker(Some(&config)),
        None,
        "an unknown token is no record"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_cards_records_are_only_ordered() {
    use aterm_durable::{Flush, seam::Recording};
    let dir = std::env::temp_dir().join(format!(
        "aterm-consent-card-flush-{}-{}",
        std::process::id(),
        aterm_time::since_epoch().unwrap().as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let config = dir.join("aterm.toml");
    let recording = Recording::start();
    record_marker(Some(&config), Marker::NotNow).unwrap();
    assert_eq!(recording.take(), [Flush::Ordered], "the answer");
    let update = AccessRecord {
        verified_at: Some(7),
        ..AccessRecord::default()
    };
    write_record(Some(&config), update).unwrap();
    assert_eq!(recording.take(), [Flush::Ordered], "the access record");
    write_verified_host(Some(&config), "HOST-UUID", "com.aterm.aterm").unwrap();
    assert_eq!(recording.take(), [Flush::Ordered], "the verified host");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_entry_line_names_the_cause_and_the_tab_it_knows() {
    let tab = NeedWords {
        cause: app_data_prompt(),
        tab: Some(TabPlace::Number(1)),
        regressed: false,
    };
    let no_tab = NeedWords { tab: None, ..tab };
    assert!(tab.event_line().contains("tab 1"));
    assert!(!no_tab.event_line().contains("tab"), "never guessed");
    let worker = NeedWords {
        tab: Some(TabPlace::Workers),
        ..tab
    };
    assert!(worker.event_line().contains("under Workers"));
    assert!(!worker.event_line().contains("tab"), "it has no number");
    let regressed = NeedWords {
        regressed: true,
        ..tab
    };
    let eperm = NeedWords {
        cause: NeedCause::ProtectedRead { session: Some(2) },
        tab: Some(TabPlace::Number(2)),
        regressed: false,
    };
    assert_ne!(regressed.event_line(), tab.event_line());
    assert_ne!(eperm.event_line(), tab.event_line());
    assert!(eperm.event_line().contains("tab 2"));
}

fn asked_once_model() -> aterm_spec::derive::Model {
    aterm_spec::ty_model! {
        AccessCardAskedOnce {
            const Buggy = 0;
            const Cap = 3;
            var gen = 0;
            var asked = 0;
            var raised = 0;
            var granted = 0;
            var need = 0;
            var opened = 0;
            var fresh = 1;
            var acted_ask = 0;
            action Need when (need == 0) { need = 1; }
            action Ask when (need == 1 && granted == 0 && asked == 0 && (opened == 0 || fresh == 1 || Buggy == 1)) {
                asked = 1; raised = raised + 1; need = 0;
                acted_ask = if opened == 1 && fresh == 0 { 1 } else { acted_ask };
            }
            action Suppress when (need == 1 && (asked == 1 || granted == 1 || (opened == 1 && fresh == 0 && Buggy == 0))) { need = 0; }
            action Open when (asked == 1 && opened == 0 && granted == 0) { opened = 1; fresh = 0; }
            action HoldLapses when (opened == 1 && fresh == 0) { opened = 0; fresh = 1; asked = 0; raised = 0; }
            action Grant when (granted == 0) { granted = 1; }
            action Regress when (granted == 1) { granted = 0; asked = 0; raised = 0; opened = 0; fresh = 1; }
            action Renew when (asked == 1 && granted == 0) { asked = 0; raised = 0; }
            action Update when (gen <= Cap - 1) {
                gen = gen + 1; need = 0; fresh = 1;
                asked = if Buggy == 1 { 0 } else { asked };
            }
            invariant AtMostOncePerEpoch: raised <= 1;
            invariant NoAskToAnOwnerWhoActedOnAnOlderWindow: acted_ask == 0;
        }
    }
}

#[test]
fn asked_once_model_proves_and_catches_the_historical_shapes() {
    let model = asked_once_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
}

struct Epochs {
    dir: std::path::PathBuf,
    config: std::path::PathBuf,
    fda: FdaState,
    now: u64,
    started: u64,
    generation: i64,
    raised: i64,
    need: bool,
    acted_ask: bool,
    /// Negative control: a process that keeps no record.
    forgets_record: bool,
    /// Negative control: an ask that ignores the window's age.
    ignores_window_age: bool,
    /// Negative control: the run departs from the model, so the model's guards are not
    /// asserted; its invariants are the check.
    lenient: bool,
}

impl Epochs {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "aterm-asked-once-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("aterm.toml");
        Self {
            dir,
            config,
            fda: FdaState::Denied,
            now: 10_000_000,
            started: 10_000_000,
            generation: 0,
            raised: 0,
            need: false,
            acted_ask: false,
            forgets_record: false,
            ignores_window_age: false,
            lenient: false,
        }
    }

    fn record(&self) -> AccessRecord {
        read_record(Some(&self.config))
    }

    fn facts(&self) -> NeedFacts {
        NeedFacts {
            card: CardFacts {
                fda: self.fda,
                ..denied_release()
            },
            now_secs: self.now,
            process_started_at: if self.ignores_window_age {
                u64::MAX
            } else {
                self.started
            },
            cause: app_data_prompt(),
        }
    }

    fn verdict(&self) -> NeedVerdict {
        decide_need(&self.facts(), None, &self.record())
    }

    fn write(&self, update: AccessRecord) {
        write_record(Some(&self.config), update).unwrap();
    }

    fn project(&self) -> std::collections::BTreeMap<&'static str, i64> {
        let record = self.record();
        let granted = self.fda == FdaState::Granted;
        let live = |at: Option<u64>, span: Duration| {
            at.is_some_and(|at| {
                self.now < at + span.as_secs()
                    && (granted || record.verified_at.is_none_or(|v| v <= at))
            })
        };
        let asked = live(record.asked_at, RENEW_AFTER);
        let opened = live(record.opened_at, OWNER_ACTED_HOLD);
        let fresh = !opened || self.started >= record.opened_at.unwrap_or(0);
        std::collections::BTreeMap::from([
            ("gen", self.generation),
            ("asked", i64::from(asked)),
            ("raised", self.raised),
            ("granted", i64::from(granted)),
            ("need", i64::from(self.need)),
            ("opened", i64::from(opened)),
            ("fresh", i64::from(fresh)),
            ("acted_ask", i64::from(self.acted_ask)),
        ])
    }

    fn apply(
        &mut self,
        model: &aterm_spec::derive::Model,
        action: &str,
        before: &std::collections::BTreeMap<&'static str, i64>,
    ) -> Result<(), String> {
        self.now += 1;
        match action {
            "Need" => self.need = true,
            "Ask" | "Suppress" => {
                self.now = self.now.max(self.started + LAUNCH_SETTLE.as_secs());
                let verdict = self.verdict();
                let asks = matches!(verdict, NeedVerdict::Ask { .. });
                if asks != (action == "Ask") {
                    return Err(format!("the real decision {verdict:?} is not {action}"));
                }
                let other = if action == "Ask" { "Suppress" } else { "Ask" };
                if !self.lenient && model.action_enabled(other, before) {
                    return Err(format!("the model allows {other} too"));
                }
                self.need = false;
                if asks {
                    self.acted_ask =
                        self.acted_ask || (before["opened"] == 1 && before["fresh"] == 0);
                    self.raised += 1;
                    self.write(AccessRecord {
                        asked_at: Some(self.now),
                        ..AccessRecord::default()
                    });
                }
            }
            "Open" => self.write(AccessRecord {
                opened_at: Some(self.now),
                ..AccessRecord::default()
            }),
            "HoldLapses" => {
                self.now += OWNER_ACTED_HOLD.as_secs() + 1;
                self.raised = 0;
            }
            "Grant" => {
                self.fda = FdaState::Granted;
                self.write(AccessRecord {
                    verified_at: Some(self.now),
                    verified_build: Some(7),
                    ..AccessRecord::default()
                });
            }
            "Regress" => {
                self.fda = FdaState::Denied;
                self.raised = 0;
            }
            "Renew" => {
                self.now += RENEW_AFTER.as_secs() + 1;
                self.raised = 0;
            }
            "Update" => {
                self.generation += 1;
                self.need = false;
                self.started = self.now;
                if self.forgets_record {
                    let _ = std::fs::remove_file(record_path(Some(&self.config)).unwrap());
                }
            }
            other => return Err(format!("unknown action {other}")),
        }
        Ok(())
    }
}

impl Drop for Epochs {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn run_epochs(
    epochs: &mut Epochs,
    actions: &[&str],
) -> std::collections::BTreeMap<&'static str, i64> {
    let model = asked_once_model();
    let mut expected = model.init_state();
    assert_eq!(epochs.project(), expected, "both start in the same state");
    for action in actions {
        let before = expected.clone();
        assert!(
            model.action_enabled(action, &before),
            "{action}: the model forbids it in {before:?}"
        );
        epochs
            .apply(&model, action, &before)
            .unwrap_or_else(|why| panic!("{action}: {why}"));
        assert!(model.fire(action, &mut expected));
        assert_eq!(
            epochs.project(),
            expected,
            "{action}: the real state departs from the model"
        );
    }
    expected
}

#[test]
fn asked_once_conforms_across_process_generations() {
    let model = asked_once_model();
    for (tag, actions) in [
        (
            "eight-updates",
            &[
                "Need", "Ask", "Update", "Need", "Suppress", "Update", "Need", "Suppress",
            ][..],
        ),
        (
            "opened-then-updated",
            &["Need", "Ask", "Open", "Update", "Need", "Suppress"][..],
        ),
        (
            "owner-acted",
            &["Need", "Ask", "Open", "Renew", "Need", "Suppress"][..],
        ),
        (
            "hold-lapses",
            &["Need", "Ask", "Open", "HoldLapses", "Need", "Ask"][..],
        ),
        (
            "grant-and-regress",
            &[
                "Need", "Ask", "Grant", "Need", "Suppress", "Regress", "Need", "Ask",
            ][..],
        ),
        (
            "renews-weekly",
            &["Need", "Ask", "Renew", "Need", "Ask"][..],
        ),
        (
            "granted-first",
            &["Grant", "Update", "Regress", "Need", "Ask"][..],
        ),
    ] {
        let mut epochs = Epochs::new(tag);
        let end = run_epochs(&mut epochs, actions);
        assert!(model.check_invariant("AtMostOncePerEpoch", &end), "{tag}");
        assert!(
            model.check_invariant("NoAskToAnOwnerWhoActedOnAnOlderWindow", &end),
            "{tag}"
        );
    }
}

#[test]
fn asked_once_catches_the_process_that_forgets_and_the_ask_that_ignores_the_window() {
    let model = asked_once_model();
    let mut forgetful = Epochs::new("forgets");
    forgetful.forgets_record = true;
    forgetful.lenient = true;
    let model_free = |epochs: &mut Epochs, action: &str| {
        let before = epochs.project();
        epochs.apply(&model, action, &before).unwrap();
    };
    for action in ["Need", "Ask", "Update", "Need", "Ask"] {
        model_free(&mut forgetful, action);
    }
    assert_eq!(forgetful.raised, 2, "the per-process card asks twice");
    assert!(
        !model.check_invariant("AtMostOncePerEpoch", &forgetful.project()),
        "the invariant reports it"
    );
    let mut blind = Epochs::new("blind");
    blind.ignores_window_age = true;
    blind.lenient = true;
    for action in ["Need", "Ask", "Open", "Renew", "Need", "Ask"] {
        model_free(&mut blind, action);
    }
    assert!(
        blind.acted_ask,
        "asked an owner who acted on an older window"
    );
    assert!(
        !model.check_invariant("NoAskToAnOwnerWhoActedOnAnOlderWindow", &blind.project()),
        "the invariant reports it"
    );
}

#[test]
fn the_access_record_round_trips_and_a_foreign_file_is_no_record() {
    let epochs = Epochs::new("record");
    let config = Some(epochs.config.as_path());
    assert_eq!(read_record(config), AccessRecord::default());
    assert!(write_record(None, AccessRecord::default()).is_err());
    let first = AccessRecord {
        asked_at: Some(100),
        opened_at: Some(200),
        verified_at: None,
        verified_build: None,
    };
    assert_eq!(write_record(config, first).unwrap(), first);
    assert_eq!(read_record(config), first);
    let merged = write_record(
        config,
        AccessRecord {
            asked_at: Some(50),
            opened_at: Some(300),
            verified_at: Some(400),
            verified_build: Some(9),
        },
    )
    .unwrap();
    assert_eq!(
        merged,
        AccessRecord {
            asked_at: Some(100),
            opened_at: Some(300),
            verified_at: Some(400),
            verified_build: Some(9),
        },
        "opened_at is written as a max"
    );
    assert_eq!(AccessRecord::parse(&merged.render()), merged);
    assert!(merged.render().len() <= MAX_RECORD_BYTES);
    let maximal = AccessRecord {
        asked_at: Some(u64::MAX),
        opened_at: Some(u64::MAX),
        verified_at: Some(u64::MAX),
        verified_build: Some(u64::MAX),
    };
    assert!(
        maximal.render().len() <= MAX_RECORD_BYTES,
        "four maximal fields still fit the bound"
    );
    let path = epochs.dir.join(RECORD);
    for foreign in [
        "hello\n",
        "asked_at=abc\n",
        "asked_at=1\nsecret=2\n",
        "asked_at=1\nnot a line\n",
    ] {
        std::fs::write(&path, foreign).unwrap();
        assert_eq!(read_record(config), AccessRecord::default(), "{foreign:?}");
    }
    std::fs::write(&path, "asked_at=1\n".repeat(MAX_RECORD_BYTES)).unwrap();
    assert_eq!(read_record(config), AccessRecord::default(), "oversized");
    std::fs::write(&path, "\nasked_at = 12\n\nverified_at=13\n").unwrap();
    assert_eq!(
        read_record(config),
        AccessRecord {
            asked_at: Some(12),
            verified_at: Some(13),
            ..AccessRecord::default()
        }
    );
}

#[test]
fn on_need_re_arms_a_settled_card_and_reoffer_no_longer_exists() {
    let t0 = Instant::now();
    let mut s = CardState::new();
    assert!(!s.on_need(t0));
    assert_eq!(s.phase(), CardPhase::Idle);
    assert!(!s.take_pending_need(), "only a settled card gives it up");
    assert!(s.begin_deciding(t0));
    assert!(!s.on_need(t0), "a verdict is coming: the need is kept");
    assert!(s.on_decided(&Verdict::Quiet(NotDue::NoNeedYet), t0));
    assert_eq!(s.phase(), CardPhase::Settled);
    assert!(s.take_pending_need(), "re-evaluated the moment it settles");
    assert!(!s.take_pending_need(), "and only once");
    assert!(s.on_need(t0 + Duration::from_secs(1)));
    assert!(matches!(s.phase(), CardPhase::Deciding { .. }));
    assert_eq!(
        s.lifecycle_deadline(),
        Some(t0 + Duration::from_secs(1) + DECIDE_TIMEOUT)
    );
    assert!(s.on_decided(&Verdict::Offer, t0));
    assert_eq!(s.phase(), CardPhase::Due);
    assert!(!s.on_need(t0));
    assert!(s.raise_allowed(t0));
    s.on_raised(t0);
    assert!(!s.on_need(t0));
    assert!(!s.take_pending_need(), "still watching");
    assert!(s.on_row_retired());
    assert_eq!(s.phase(), CardPhase::Settled);
    assert_eq!(s.lifecycle_deadline(), None);
    assert!(
        s.take_pending_need(),
        "the need kept while watching returns"
    );
    let mut lapses = CardState::new();
    lapses.settle();
    assert!(lapses.on_need(t0));
    for attempt in 1..=DECIDE_ATTEMPTS {
        assert!(lapses.deciding_lapsed(t0 + DECIDE_TIMEOUT));
        assert_eq!(lapses.reopen_deciding(), attempt < DECIDE_ATTEMPTS);
        if attempt < DECIDE_ATTEMPTS {
            assert!(lapses.begin_deciding(t0));
        }
    }
    assert_eq!(lapses.phase(), CardPhase::Settled);
}

/// Re-arming restarts the launch check from `Idle` with a fresh attempt budget, and only from
/// `Settled`.
#[test]
fn a_launch_check_a_switch_held_rearms_from_settled_only() {
    let t0 = Instant::now();
    let mut held = CardState::new();
    assert!(
        !held.admit_current_policy(true, false),
        "notice off at launch"
    );
    assert_eq!(held.phase(), CardPhase::Settled);
    assert!(held.rearm_launch());
    assert_eq!(held.phase(), CardPhase::Idle);
    assert!(held.begin_deciding(t0), "the launch check runs again");

    let mut mid_read = CardState::new();
    assert!(mid_read.begin_deciding(t0));
    assert!(!mid_read.admit_current_policy(false, true), "probe off");
    assert!(mid_read.rearm_launch());
    for attempt in 1..=DECIDE_ATTEMPTS {
        assert!(mid_read.begin_deciding(t0), "attempt {attempt} starts");
        assert!(mid_read.deciding_lapsed(t0 + DECIDE_TIMEOUT));
        assert_eq!(
            mid_read.reopen_deciding(),
            attempt < DECIDE_ATTEMPTS,
            "a fresh budget: attempt {attempt} of {DECIDE_ATTEMPTS}"
        );
    }

    for mut busy in [CardState::new(), {
        let mut deciding = CardState::new();
        assert!(deciding.begin_deciding(t0));
        deciding
    }] {
        let phase = busy.phase();
        assert!(!busy.rearm_launch(), "{phase:?}: nothing to run again");
        assert_eq!(busy.phase(), phase);
    }
    let mut due = CardState::new();
    assert!(due.begin_deciding(t0));
    assert!(due.on_decided(&Verdict::Offer, t0));
    assert!(!due.rearm_launch(), "a due card keeps its question");
    assert_eq!(due.phase(), CardPhase::Due);
}

#[test]
fn a_swallowed_verdict_is_asked_for_again_and_then_given_up_on() {
    let t0 = Instant::now();
    let mut s = CardState::new();
    for attempt in 1..=DECIDE_ATTEMPTS {
        assert!(s.begin_deciding(t0), "attempt {attempt} starts");
        assert!(
            !s.deciding_lapsed(t0.checked_sub(Duration::from_millis(1)).unwrap_or(t0)),
            "an older cached frame instant never panics or lapses"
        );
        assert!(!s.deciding_lapsed(t0), "not lapsed on the same instant");
        assert!(!s.deciding_lapsed(t0 + DECIDE_TIMEOUT - Duration::from_millis(1)));
        assert!(s.deciding_lapsed(t0 + DECIDE_TIMEOUT));
        let again = s.reopen_deciding();
        assert_eq!(
            again,
            attempt < DECIDE_ATTEMPTS,
            "attempt {attempt} of {DECIDE_ATTEMPTS}"
        );
    }
    assert_eq!(s.phase(), CardPhase::Settled, "the attempts are spent");
    assert_eq!(s.lifecycle_deadline(), None);
    assert!(!s.begin_deciding(t0), "and it does not start again");
    assert!(
        !s.on_decided(&Verdict::Offer, t0),
        "a late verdict is stale"
    );
    let mut ok = CardState::new();
    assert!(ok.begin_deciding(t0));
    assert!(ok.on_decided(&Verdict::Offer, t0));
    assert_eq!(ok.phase(), CardPhase::Due);
}

#[test]
fn the_lifecycle_decides_once_watches_for_a_while_and_settles() {
    let t0 = Instant::now();
    let mut s = CardState::new();
    assert_eq!(s.phase(), CardPhase::Idle);
    assert!(!s.wants_probe(t0), "idle: nothing to watch");
    assert!(!s.on_row_retired(), "idle: no row to lose");
    assert!(s.begin_deciding(Instant::now()));
    assert!(!s.begin_deciding(Instant::now()), "the one-shot fires once");
    assert!(matches!(s.phase(), CardPhase::Deciding { .. }));
    s.on_raised(t0);
    assert!(
        matches!(s.phase(), CardPhase::Deciding { .. }),
        "nothing to raise yet"
    );

    assert!(s.on_decided(&Verdict::Offer, t0));
    assert_eq!(s.phase(), CardPhase::Due);
    assert!(
        !s.on_decided(&Verdict::Offer, t0),
        "a second verdict is stale"
    );
    assert!(!s.wants_probe(t0), "due but not posted: no watch yet");
    assert!(!s.on_row_retired(), "not posted: no row to lose");
    assert!(s.raise_allowed(t0), "the first raise is always allowed");
    s.on_raised(t0);
    assert_eq!(s.phase(), CardPhase::Watching { since: t0 });
    assert!(s.wants_probe(t0 + Duration::from_secs(60)));
    assert!(!s.raise_allowed(t0), "posted once; never twice");

    let mut d = CardState::new();
    assert!(d.begin_deciding(Instant::now()));
    assert!(d.on_decided(&Verdict::Offer, t0));
    assert!(d.raise_allowed(t0 + WATCH_FOR - Duration::from_secs(1)));
    assert!(!d.raise_allowed(t0 + WATCH_FOR));
    assert_eq!(d.phase(), CardPhase::Settled);
    assert_eq!(d.lifecycle_deadline(), None);

    let mut c = CardState::new();
    assert!(c.begin_deciding(Instant::now()));
    assert!(c.on_decided(&Verdict::Confirm, t0));
    assert_eq!(c.phase(), CardPhase::Settled);
    assert_eq!(c.lifecycle_deadline(), None);

    let t2 = t0 + Duration::from_secs(20);
    let mut o = CardState::new();
    assert!(o.begin_deciding(Instant::now()));
    assert!(o.on_decided(&Verdict::Offer, t0));
    o.on_raised(t0);
    o.on_opened_settings(t2);
    assert_eq!(o.phase(), CardPhase::Watching { since: t2 });
    assert!(!o.on_row_retired());
    assert_eq!(o.phase(), CardPhase::Watching { since: t2 });
    assert!(o.wants_probe(t2 + WATCH_FOR - Duration::from_secs(1)));
    assert!(!o.wants_probe(t2 + WATCH_FOR), "the watch is bounded");
    assert_eq!(o.phase(), CardPhase::Settled);
    o.on_opened_settings(t2);
    assert_eq!(
        o.phase(),
        CardPhase::Watching { since: t2 },
        "a new Settings gesture starts a fresh bounded watch"
    );
    assert!(!o.wants_probe(t2 + WATCH_FOR));
    assert_eq!(
        o.lifecycle_deadline(),
        None,
        "an explicit gesture never reoffers the card"
    );

    let mut b = CardState::new();
    assert!(b.begin_deciding(Instant::now()));
    assert!(b.on_decided(&Verdict::Offer, t0));
    b.on_raised(t0);
    b.on_owner_acted();
    assert!(b.wants_probe(t0 + Duration::from_secs(1)));
    assert!(!b.on_row_retired());
    assert_eq!(b.phase(), CardPhase::Watching { since: t0 });
    assert!(!b.wants_probe(t0 + WATCH_FOR));
    assert_eq!(b.lifecycle_deadline(), None, "read rows do not return");

    let mut n = CardState::new();
    assert!(n.begin_deciding(Instant::now()));
    assert!(n.on_decided(&Verdict::Quiet(NotDue::Granted), t0));
    assert_eq!(n.phase(), CardPhase::Settled);

    let mut w = CardState::new();
    assert!(w.begin_deciding(Instant::now()));
    assert!(w.on_decided(&Verdict::Offer, t0));
    w.on_raised(t0);
    w.settle();
    assert_eq!(w.phase(), CardPhase::Settled);
    assert!(!w.wants_probe(t0));
    assert!(!w.on_row_retired());
}
