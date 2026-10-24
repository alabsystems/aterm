// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::supervise::policy::turn_end::ComposerRead;

fn active_claim(s: &mut Session<'_, Mock>) {
    active_metadata_claim(s);
}

fn box_point(s: &mut Session<'_, Mock>) -> Turn {
    let screen = s.screen().expect("the box");
    s.turn_of(screen)
}

fn requested_badge(m: &Mock) -> String {
    let text = m
        .requests
        .iter()
        .rev()
        .find_map(|r| r.strip_prefix("meta set attention owner=supervisor "))
        .expect("the requested native badge");
    text.strip_prefix("kind=ask ")
        .or_else(|| text.strip_prefix("kind=wait "))
        .unwrap_or(text)
        .to_string()
}

fn wait_metadata(s: &mut Session<'_, Mock>, point: &Turn) {
    let mut deadline = s.fx.mono() + Duration::from_millis(450);
    let _ = s
        .wait_for_next(point.clone(), &auto(30), &[], &mut deadline, &mut Alone)
        .expect("the metadata wait");
}

#[test]
fn refused_supervisor_badges_recover_on_an_unchanged_point() {
    for limit in [false, true] {
        let mut m = Mock::new(vec![if limit {
            limited_screen()
        } else {
            question_screen()
        }]);
        m.autopilot = Some("cruise");
        m.key_releases = Some(0);
        m.stall_sleep = Some(Duration::from_secs(60));
        let point;
        {
            let mut s = virtual_session(&mut m, None);
            active_claim(&mut s);
            point = box_point(&mut s);
            s.ctl
                .attention_replies
                .push_back(err("attention owners full (max 8): unset one first"));
            s.ctl
                .attention_replies
                .push_back(err("attention owners full (max 8): unset one first"));
            s.setup.redecide_after_skip = Duration::ZERO;
            let began = s.fx.mono();
            if limit {
                s.open_episode(point.screen.seq, "the limit", None, false, &mut Alone)
                    .expect("the limit");
            } else {
                s.escalate_point(&point, Some("the person is needed"), &mut Alone)
                    .expect("the box");
            }
            let mut deadline = s.fx.mono() + Duration::from_millis(450);
            let end = s
                .wait_for_next(point, &auto(30), &[], &mut deadline, &mut Alone)
                .expect("the unchanged point");
            assert!(matches!(end, Some(End::Timeout(_))));
            assert!(
                s.ctl.attention_at.is_some_and(
                    |at| at.saturating_duration_since(began) >= Duration::from_millis(200)
                ),
                "both rejections paced before success"
            );
        }
        assert_eq!(
            m.attentions, 3,
            "two rejected SETs and their acknowledged retry: {:#?}",
            m.requests
        );
        assert!(m.attention.is_some(), "the native badge was delivered");
        assert!(
            !m.requests
                .iter()
                .any(|r| claim::is_input(&r.split_whitespace().collect::<Vec<_>>()))
        );
    }
}

#[test]
fn an_applied_supervisor_set_with_a_lost_reply_is_still_cleared() {
    for limit in [false, true] {
        let mut m = Mock::new(vec![question_screen()]);
        {
            let mut s = virtual_session(&mut m, None);
            active_claim(&mut s);
            let point = box_point(&mut s);
            s.ctl.replies.push_back(replaced("the SET reply was lost"));
            if limit {
                s.open_episode(point.screen.seq, "the limit", None, false, &mut Alone)
                    .expect("set");
            } else {
                s.escalate_point(&point, Some("the person is needed"), &mut Alone)
                    .expect("set");
            }
            // Server state is independent of the lost acknowledgement.
            s.ctl.attention = Some(requested_badge(s.ctl));
            if limit {
                s.close_episode(point.screen.seq, "resolved", &mut Alone)
                    .expect("close");
            } else {
                s.close_box(point.screen.seq, &mut Alone).expect("close");
            }
        }
        assert_eq!(
            m.attention, None,
            "a lost SET acknowledgement did not lose cleanup: {:#?}",
            m.requests
        );
    }
}

#[test]
fn cleanup_intent_survives_consumed_flags_into_off_and_release() {
    for park in [false, true] {
        let mut m = Mock::new(vec![question_screen()]);
        let mut s = virtual_session(&mut m, None);
        active_claim(&mut s);
        let point = box_point(&mut s);
        s.escalate_point(&point, Some("a decision"), &mut Alone)
            .expect("set");
        s.ctl
            .replies
            .push_back(replaced("the clear reply was lost"));
        s.close_box(point.screen.seq, &mut Alone)
            .expect("first clear");
        assert!(s.box_ask.is_none());
        if park {
            s.lower_badges(point.screen.seq, &mut Alone)
                .expect("Off cleanup");
        } else {
            s.release_attention(&mut Alone);
        }
        assert_eq!(
            s.ctl.attention, None,
            "cleanup does not rely on consumed flags"
        );
    }
}

#[test]
fn a_stop_during_fresh_ownership_read_retains_cleanup_for_release() {
    for clearing in [false, true] {
        let mut m = Mock::new(vec![question_screen()]);
        m.autopilot = Some("cruise");
        m.key_releases = Some(0);
        m.stall_sleep = Some(Duration::from_secs(60));
        let mut s = virtual_session(&mut m, None);
        active_claim(&mut s);
        let point = box_point(&mut s);
        if !clearing {
            s.ctl.replies.push_back(replaced("the SET reply was lost"));
        }
        s.escalate_point(&point, Some("a decision"), &mut Alone)
            .expect("set attempted");
        s.ctl.attention = Some(requested_badge(s.ctl));
        if clearing {
            s.ctl
                .replies
                .push_back(replaced("the clear reply was lost"));
            s.close_box(point.screen.seq, &mut Alone)
                .expect("clear attempted");
            assert!(s.box_ask.is_none());
        }
        let stop = Arc::new(AtomicBool::new(false));
        s.stop = Some(Arc::clone(&stop));
        s.ctl.stop_on_meta = Some((s.ctl.metas + 1, Arc::clone(&stop)));
        wait_metadata(&mut s, &point);
        assert!(
            stop.load(Ordering::SeqCst),
            "the read actually observed the stop"
        );
        s.release_attention(&mut Alone);
        assert_eq!(
            s.ctl.attention, None,
            "stopping a retry retains ordinary cleanup custody"
        );
    }
}

#[test]
fn missing_mode_defers_a_set_until_a_fresh_mode_is_known() {
    for mode in [None, Some("unrecognized")] {
        let mut m = Mock::new(vec![question_screen()]);
        m.autopilot = Some("cruise");
        m.key_releases = Some(0);
        m.stall_sleep = Some(Duration::from_secs(60));
        let mut s = virtual_session(&mut m, None);
        active_claim(&mut s);
        let point = box_point(&mut s);
        s.ctl.replies.push_back(err("attention owners full"));
        s.escalate_point(&point, Some("a decision"), &mut Alone)
            .expect("set attempted");
        s.ctl.autopilot = mode;
        wait_metadata(&mut s, &point);
        assert_eq!(
            s.ctl.attentions, 1,
            "holder proof alone does not prove Autopilot"
        );
        s.ctl.autopilot = Some("cruise");
        wait_metadata(&mut s, &point);
        assert_eq!(
            s.ctl.attentions, 2,
            "a known mode restores delivery at the same point"
        );
    }
}

#[test]
fn an_absent_lease_keeps_intent_for_an_ordinary_claim_without_timer_acquisition() {
    for clear in [false, true] {
        let mut m = Mock::new(vec![question_screen()]);
        m.autopilot = Some("cruise");
        m.key_releases = Some(0);
        m.stall_sleep = Some(Duration::from_secs(60));
        let mut s = virtual_session(&mut m, None);
        active_claim(&mut s);
        let point = box_point(&mut s);
        if !clear {
            s.ctl.replies.push_back(err("attention owners full"));
        }
        s.escalate_point(&point, Some("a decision"), &mut Alone)
            .expect("set attempted");
        if clear {
            s.ctl
                .replies
                .push_back(replaced("the clear reply was lost"));
            s.close_box(point.screen.seq, &mut Alone)
                .expect("clear attempted");
        }
        s.ctl.supervisor_owner = None;
        let claims = s.ctl.claims.len();
        wait_metadata(&mut s, &point);
        assert_eq!(s.ctl.claims.len(), claims, "the timer acquired no lease");
        // The active loop, outside the timer, later owns an acknowledged lease.
        active_claim(&mut s);
        wait_metadata(&mut s, &point);
        assert_eq!(
            s.ctl.attention.is_none(),
            clear,
            "the original intent survived missing proof"
        );
    }
}

#[test]
fn configured_beside_mode_retires_a_pending_badge_before_the_enum_changes() {
    let mut m = Mock::new(vec![question_screen()]);
    m.autopilot = Some("cruise");
    m.key_releases = Some(0);
    m.stall_sleep = Some(Duration::from_secs(60));
    let mut s = virtual_session(&mut m, None);
    active_claim(&mut s);
    let point = box_point(&mut s);
    s.ctl.replies.push_back(err("attention owners full"));
    s.escalate_point(&point, Some("a decision"), &mut Alone)
        .expect("set attempted");
    s.set_beside(Some("codex-alab@4242".to_string()));
    s.ctl.attention = Some("the self-claim's badge".to_string());
    wait_metadata(&mut s, &point);
    assert_eq!(s.ctl.attention.as_deref(), Some("the self-claim's badge"));
    assert_eq!(s.ctl.attentions, 1);
}

#[test]
fn an_immediate_close_cannot_clear_a_successor_using_a_recent_local_lease() {
    for limit in [false, true] {
        let mut m = Mock::new(vec![question_screen()]);
        let mut s = virtual_session(&mut m, None);
        active_claim(&mut s);
        let point = box_point(&mut s);
        if limit {
            s.open_episode(point.screen.seq, "the limit", None, false, &mut Alone)
                .expect("set");
        } else {
            s.escalate_point(&point, Some("a decision"), &mut Alone)
                .expect("set");
        }
        // A replacement already has a successor before the old lease's renewal is due.
        s.ctl.supervisor_owner = Some("the-successor".to_string());
        s.ctl.attention = Some("the successor's badge".to_string());
        if limit {
            s.close_episode(point.screen.seq, "resolved", &mut Alone)
                .expect("close");
        } else {
            s.close_box(point.screen.seq, &mut Alone).expect("close");
        }
        assert_eq!(s.ctl.attention.as_deref(), Some("the successor's badge"));
        assert_eq!(count(s.ctl, "meta unset attention owner=supervisor"), 0);
    }
}

#[test]
fn a_lost_supervisor_clear_outlives_the_consumed_box_or_limit_flag() {
    for limit in [false, true] {
        let mut m = Mock::new(vec![question_screen()]);
        m.autopilot = Some("cruise");
        m.key_releases = Some(0);
        m.stall_sleep = Some(Duration::from_secs(60));
        {
            let mut s = virtual_session(&mut m, None);
            active_claim(&mut s);
            let point = box_point(&mut s);
            if limit {
                s.open_episode(point.screen.seq, "the limit", None, false, &mut Alone)
                    .expect("set");
            } else {
                s.escalate_point(&point, Some("the person is needed"), &mut Alone)
                    .expect("set");
            }
            s.ctl
                .replies
                .push_back(replaced("the clear was not acknowledged"));
            if limit {
                s.close_episode(point.screen.seq, "resolved", &mut Alone)
                    .expect("close");
                assert!(s.limit.is_none());
            } else {
                s.close_box(point.screen.seq, &mut Alone).expect("close");
                assert!(s.box_ask.is_none());
            }
            let mut deadline = s.fx.mono() + Duration::from_millis(450);
            let end = s
                .wait_for_next(point, &auto(30), &[], &mut deadline, &mut Alone)
                .expect("the unchanged point");
            assert!(matches!(end, Some(End::Timeout(_))));
        }
        assert_eq!(
            m.attention, None,
            "the resolved badge was eventually removed: {:#?}",
            m.requests
        );
        assert_eq!(count(&m, "meta unset attention owner=supervisor"), 2);
    }
}

#[test]
fn a_recent_needs_lease_cannot_write_through_a_successor_after_replacement() {
    for delayed in [false, true] {
        for clear in [false, true] {
            let mut m = Mock::new(vec![question_screen()]);
            m.autopilot = Some("cruise");
            m.key_releases = Some(0);
            m.stall_sleep = Some(Duration::from_secs(60));
            {
                let mut s = virtual_session(&mut m, None);
                active_claim(&mut s);
                let point = box_point(&mut s);
                let owner = "owner=needs";
                s.turn_end.observe(
                    &TurnEndReading {
                        said_tail: Some("NEEDS YOU: the signing credential".to_string()),
                        worked: Some(Duration::from_secs(60)),
                        ..TurnEndReading::of(
                            &s.reader(&point.screen.rows).read(&point.screen.rows, None),
                            &point.screen.rows,
                            ComposerRead::default(),
                            None,
                            None,
                            None,
                        )
                    },
                    s.fx.mono(),
                );
                assert_eq!(s.turn_end.standing_needs(), ["the signing credential"]);
                s.ctl
                    .needs_replies
                    .push_back(replaced("the needs write was not acknowledged"));
                s.sync_needs(&point.screen, &auto(30), &mut Alone)
                    .expect("the pending write");
                if clear {
                    s.turn_end.park_off();
                }
                s.ctl.supervisor_owner = Some("the-successor".to_string());
                s.ctl.needs_attention = Some("the successor's need".to_string());
                if delayed {
                    wait_metadata(&mut s, &point);
                } else {
                    s.sync_needs(&point.screen, &auto(30), &mut Alone)
                        .expect("the immediate metadata update");
                }
                assert_eq!(
                    s.ctl.needs_attention.as_deref(),
                    Some("the successor's need")
                );
                assert_eq!(
                    s.ctl.needs_calls.len(),
                    1,
                    "no delayed {owner} mutation after ownership changed"
                );
            }
        }
    }
}

#[test]
fn unknown_local_claims_stay_paced_without_timer_acquisition() {
    for unavailable in [false, true] {
        let mut m = Mock::new(vec![question_screen()]);
        m.autopilot = Some("cruise");
        let mut s = virtual_session(&mut m, None);
        active_claim(&mut s);
        let point = box_point(&mut s);
        s.ctl.replies.push_back(err("attention owners full"));
        s.escalate_point(&point, Some("a decision"), &mut Alone)
            .expect("set attempted");
        s.claim = if unavailable {
            claim::Claim::Unavailable {
                why: "the old host did not answer claims".to_string(),
            }
        } else {
            claim::Claim::Pending { tried: s.fx.mono() }
        };
        let claims = s.ctl.claims.len();
        s.retry_supervisor_attention(&point.screen, &auto(30), &mut Alone)
            .expect("the deferred timer");
        assert_eq!(s.ctl.claims.len(), claims);
        assert_eq!(s.ctl.attentions, 1);
        assert!(s.attention_pending && s.attention_retry_at.is_some_and(|at| at > s.fx.mono()));
    }
}

#[test]
fn an_explicitly_unset_mode_resolves_the_global_policy() {
    for global in [Autopilot::Cruise, Autopilot::Off] {
        let mut m = Mock::new(vec![question_screen()]);
        m.autopilot = Some("cruise");
        m.key_releases = Some(0);
        m.stall_sleep = Some(Duration::from_secs(60));
        let mut s = virtual_session(&mut m, None);
        active_claim(&mut s);
        let point = box_point(&mut s);
        s.ctl.replies.push_back(err("attention owners full"));
        s.escalate_point(&point, Some("a decision"), &mut Alone)
            .expect("set attempted");
        s.ctl.autopilot = Some("-");
        let mut opts = auto(30);
        opts.policy.autopilot = global;
        let mut deadline = s.fx.mono() + Duration::from_millis(450);
        let _ = s
            .wait_for_next(point, &opts, &[], &mut deadline, &mut Alone)
            .expect("the unchanged wait");
        assert_eq!(s.ctl.attention.is_some(), global == Autopilot::Cruise);
        assert!(!s.attention_pending);
    }
}

fn ordinary_fresh_global_supervisor(global: Autopilot) {
    let mut m = Mock::new(vec![question_screen()]);
    m.autopilot = Some("cruise");
    m.autopilot_at = vec![(3, Some("-"))];
    m.vanish_after = Some(0);
    let mut opts = auto(30);
    opts.policy.autopilot = global;
    let (lines, code) = watch_lines(&mut m, &opts);
    assert_eq!(code, 1, "{lines:#?}");
    assert!(
        lines
            .last()
            .is_some_and(|line| line.starts_with("EXIT session gone ")),
        "the fixture ended by disappearance: {lines:#?}"
    );
    assert!(
        lines.iter().any(|line| line.starts_with("EVENT question ")),
        "the ordinary decision was reached: {lines:#?}"
    );
    assert!(m.metas >= 3 && m.autopilot == Some("-"));
    assert!(
        m.claims
            .first()
            .is_some_and(|request| request.starts_with("meta set supervisor "))
    );
    assert!(
        !m.requests
            .iter()
            .any(|r| claim::is_input(&r.split_whitespace().collect::<Vec<_>>()))
    );
    let sets = count(&m, "meta set attention owner=supervisor");
    assert_eq!(
        sets,
        usize::from(global != Autopilot::Off),
        "fresh global mode controls the first ordinary SET: {:#?}",
        m.requests
    );
    if global != Autopilot::Off {
        let set = m
            .requests
            .iter()
            .position(|r| r.starts_with("meta set attention owner=supervisor"))
            .expect("the immediate publication");
        assert_eq!(m.requests[set - 1], "meta");
        assert_eq!(
            m.requests[..set].iter().filter(|r| *r == "meta").count(),
            3,
            "publication succeeded before any retry wait"
        );
    }
}

#[test]
fn ordinary_metadata_fresh_global_off_withholds_supervisor() {
    ordinary_fresh_global_supervisor(Autopilot::Off);
}

#[test]
fn ordinary_metadata_fresh_global_cruise_publishes_supervisor() {
    ordinary_fresh_global_supervisor(Autopilot::Cruise);
}

fn cap_reply_stops_an_unclaimed_publication(hand_over: bool) {
    let mut m = Mock::new(vec![question_screen()]);
    let mut s = virtual_session(&mut m, None);
    assert!(matches!(s.claim, claim::Claim::Off));
    s.attention_desired = Some(escalate::AttentionTarget::Set {
        text: "a decision whose attention words exceed the advertised forty byte cap".to_string(),
        kind: escalate::Escalation::Ask,
    });
    s.ctl
        .attention_replies
        .push_back(err("attention too long (max 40 bytes)"));
    let stop = Arc::new(AtomicBool::new(false));
    let handover = Arc::new(AtomicBool::new(false));
    s.stop = Some(Arc::clone(&stop));
    s.set_handover(Arc::clone(&handover));
    let after = Arc::clone(&stop);
    s.ctl.after_call = Some(Box::new(move || {
        handover.store(hand_over, Ordering::SeqCst);
        after.store(true, Ordering::SeqCst);
    }));
    let _ = s.write_attention_target();
    assert!(stop.load(Ordering::SeqCst), "the cap reply raised stop");
    assert_eq!(
        (
            s.ctl.attention.clone(),
            s.ctl.attentions,
            s.attention_pending,
            s.attention_ours.clone(),
        ),
        (None, 1, true, None),
        "no second write may follow the observed stop: {:#?}",
        s.ctl.requests
    );
    assert!(s.ctl.claims.is_empty(), "the legacy call acquired no claim");
}

#[test]
fn ordinary_attention_cap_reply_stops_an_unclaimed_set() {
    cap_reply_stops_an_unclaimed_publication(false);
}

#[test]
fn ordinary_attention_cap_reply_hands_an_unclaimed_set_over() {
    cap_reply_stops_an_unclaimed_publication(true);
}

#[test]
fn desired_kind_and_parked_text_do_not_fall_back_to_an_old_acknowledgement() {
    let mut m = Mock::new(vec![question_screen()]);
    let mut s = virtual_session(&mut m, None);
    active_claim(&mut s);
    let point = box_point(&mut s);
    s.open_episode(point.screen.seq, "the old limit", None, false, &mut Alone)
        .expect("the old badge");
    s.ctl
        .replies
        .push_back(replaced("the new badge's reply was lost"));
    s.escalate_point(&point, Some("the new decision"), &mut Alone)
        .expect("the new badge");
    let current = requested_badge(s.ctl);
    s.ctl.attention = Some(current.clone());
    s.close_episode(point.screen.seq, "the limit resolved", &mut Alone)
        .expect("the old kind closed");
    assert_eq!(
        s.ctl.attention.as_ref(),
        Some(&current),
        "closing the old limit keeps the current question"
    );
    s.close_box(point.screen.seq, &mut Alone)
        .expect("the current question closed");
    assert_eq!(s.ctl.attention, None);

    let mut m = Mock::new(vec![question_screen()]);
    let mut s = virtual_session(&mut m, None);
    active_claim(&mut s);
    s.escalate(
        101,
        "the old unsent prompt",
        None,
        super::super::escalate::Escalation::Ask,
        &mut Alone,
    )
    .expect("old SET");
    s.ctl
        .replies
        .push_back(replaced("the newer prompt's SET reply was lost"));
    s.escalate(
        101,
        "the current unsent prompt",
        None,
        super::super::escalate::Escalation::Ask,
        &mut Alone,
    )
    .expect("new SET");
    s.turn_end_badge = true;
    s.lower_badges(101, &mut Alone).expect("Off cleanup");
    assert_eq!(s.badge_parked.as_deref(), Some("the current unsent prompt"));
}

#[test]
fn a_transport_error_before_caller_bookkeeping_keeps_final_cleanup() {
    for park in [false, true] {
        let mut m = Mock::new(vec![question_screen()]);
        let mut s = virtual_session(&mut m, None);
        active_claim(&mut s);
        let point = box_point(&mut s);
        s.ctl.hang_up_at = Some("meta set attention owner=supervisor");
        assert!(
            s.escalate_point(&point, Some("a decision"), &mut Alone)
                .is_err()
        );
        assert!(s.box_ask.is_none());
        s.ctl.attention = Some(
            s.attention_text()
                .expect("the unacknowledged desired badge")
                .to_string(),
        );
        s.ctl.hang_up_at = None;
        s.ctl.hung_up = false;
        if park {
            s.lower_badges(point.screen.seq, &mut Alone)
                .expect("Off cleanup");
        } else {
            s.release_attention(&mut Alone);
        }
        assert_eq!(s.ctl.attention, None);
    }
}

#[test]
fn fresh_off_between_a_decision_and_publication_withholds_the_badge() {
    let mut m = Mock::new(vec![question_screen()]);
    m.autopilot = Some("cruise");
    let mut s = virtual_session(&mut m, None);
    active_claim(&mut s);
    let point = box_point(&mut s);
    s.ctl.autopilot = Some("off");
    let mut review = Noted::default();
    s.escalate_point(&point, Some("the earlier decision"), &mut review)
        .expect("publication considered");
    assert_eq!(s.ctl.attentions, 0, "fresh Off publishes no new badge");
    assert_eq!(s.ctl.attention, None);
    assert!(
        review
            .0
            .iter()
            .any(|line| line.contains("attention=skipped:"))
    );
}

#[test]
fn handover_during_an_ordinary_close_keeps_the_native_badge_for_the_successor() {
    let mut m = Mock::new(vec![question_screen()]);
    m.autopilot = Some("cruise");
    let mut s = virtual_session(&mut m, None);
    active_claim(&mut s);
    let point = box_point(&mut s);
    s.escalate_point(&point, Some("a decision"), &mut Alone)
        .expect("set");
    let badge = s.ctl.attention.clone();
    let stop = Arc::new(AtomicBool::new(false));
    s.stop = Some(Arc::clone(&stop));
    s.set_handover(Arc::new(AtomicBool::new(true)));
    s.ctl.stop_on_meta = Some((s.ctl.metas + 1, Arc::clone(&stop)));
    s.close_box(point.screen.seq, &mut Alone)
        .expect("close considered");
    assert!(stop.load(Ordering::SeqCst));
    assert_eq!(
        s.ctl.attention, badge,
        "the handover preserves native attention"
    );
    assert_eq!(count(s.ctl, "meta unset attention owner=supervisor"), 0);
}

fn unproved_after_give_back(s: &mut Session<'_, Mock>, state: usize) {
    s.give_back_claim();
    assert!(matches!(s.claim, claim::Claim::GivenBack { .. }));
    match state {
        0 => {}
        1 => s.claim = claim::Claim::Pending { tried: s.fx.mono() },
        2 => {
            s.claim = claim::Claim::Unavailable {
                why: "ordinary renewal was not confirmed".to_string(),
            };
        }
        _ => {
            s.claim = claim::Claim::Off;
            s.set_beside(Some("the-successor".to_string()));
        }
    }
    s.ctl.supervisor_owner = Some("the-successor".to_string());
}

#[test]
fn final_supervisor_cleanup_cannot_write_after_relinquished_or_unconfirmed_custody() {
    for state in 0..4 {
        let mut m = Mock::new(vec![question_screen()]);
        m.autopilot = Some("cruise");
        let mut s = virtual_session(&mut m, None);
        active_claim(&mut s);
        let point = box_point(&mut s);
        s.escalate_point(&point, Some("a decision"), &mut Alone)
            .expect("set");
        s.ctl.autopilot = Some("off");
        for _ in 0..2 {
            s.ctl.attention_replies.push_back(err("cleanup refused"));
        }
        s.lower_badges(point.screen.seq, &mut Alone)
            .expect("failed Off cleanup retained");
        assert!(s.attention_pending);
        unproved_after_give_back(&mut s, state);
        s.ctl.attention = Some("the successor's badge".to_string());
        let unsets = count(s.ctl, "meta unset attention owner=supervisor");
        s.release_attention(&mut Alone);
        assert_eq!(s.ctl.attention.as_deref(), Some("the successor's badge"));
        assert_eq!(
            count(s.ctl, "meta unset attention owner=supervisor"),
            unsets
        );
    }
}

#[test]
fn final_needs_cleanup_cannot_write_after_relinquished_or_unconfirmed_custody() {
    for state in 0..4 {
        let mut m = Mock::new(vec![question_screen()]);
        m.autopilot = Some("cruise");
        let mut s = virtual_session(&mut m, None);
        active_claim(&mut s);
        let point = box_point(&mut s);
        s.turn_end.observe(
            &TurnEndReading {
                said_tail: Some("NEEDS YOU: the signing credential".to_string()),
                worked: Some(Duration::from_secs(60)),
                ..TurnEndReading::of(
                    &s.reader(&point.screen.rows).read(&point.screen.rows, None),
                    &point.screen.rows,
                    ComposerRead::default(),
                    None,
                    None,
                    None,
                )
            },
            s.fx.mono(),
        );
        s.sync_needs(&point.screen, &auto(30), &mut Alone)
            .expect("set");
        s.ctl.autopilot = Some("off");
        s.ctl.needs_replies.push_back(err("cleanup refused"));
        s.lower_badges(point.screen.seq, &mut Alone)
            .expect("failed Off cleanup retained");
        assert!(s.needs_pending);
        unproved_after_give_back(&mut s, state);
        s.ctl.needs_attention = Some("the successor's need".to_string());
        let calls = s.ctl.needs_calls.len();
        s.release_attention(&mut Alone);
        assert_eq!(
            s.ctl.needs_attention.as_deref(),
            Some("the successor's need")
        );
        assert_eq!(s.ctl.needs_calls.len(), calls);
    }
}

#[test]
fn separate_metadata_proof_outages_get_separate_recovery_windows() {
    proof_outages(false);
}

#[test]
fn separate_metadata_transport_errors_get_separate_recovery_windows() {
    proof_outages(true);
}

fn proof_outages(host_error: bool) {
    let mut m = Mock::new(vec![question_screen()]);
    m.autopilot = Some("cruise");
    let mut s = virtual_session(&mut m, None);
    active_claim(&mut s);
    let claims = s.ctl.claims.len();
    let point = box_point(&mut s);
    s.set_reconnect(Duration::from_millis(100));
    s.setup.pause = Duration::from_millis(10);
    let mut lines = Vec::new();
    let mut recovered = Vec::new();
    let mut cleared = Vec::new();
    for round in 0..2 {
        if host_error {
            s.ctl.hang_up_at = Some("meta");
            s.ctl.hung_up = false;
        } else {
            s.ctl
                .verb_replies
                .entry("meta")
                .or_default()
                .push_back(closed());
        }
        let why = match s.metadata_authority(false) {
            Err(Fail::Lost(why)) => why,
            _ => panic!("the proof read must lose its reply"),
        };
        s.ctl.hang_up_at = None;
        s.ctl.hung_up = false;
        let deadline = s.fx.mono() + Duration::from_secs(1);
        let back = s.ride_out(why, deadline, &mut |line| {
            lines.push(line.to_string());
            Ok(())
        });
        recovered.push(matches!(back, Ok(Rode::Back { .. })));
        assert!(matches!(
            s.metadata_authority(false),
            Ok(claim::MetadataAuthority::Ours)
        ));
        cleared.push(s.outage.is_none());
        if round == 0 {
            s.fx.sleep(Duration::from_millis(200));
        }
    }
    assert_eq!(
        recovered,
        [true, true],
        "each proof read gets its own window: {lines:#?}"
    );
    assert_eq!(
        cleared,
        [true, true],
        "successful proof reads end their outage"
    );
    assert_eq!(
        point.screen.rows,
        s.screen().expect("the unchanged prompt").rows
    );
    assert_eq!(count(s.ctl, "await seq"), 0);
    assert_eq!(
        s.ctl.claims.len(),
        claims,
        "proof reads never claim or renew"
    );
    assert!(
        !s.ctl
            .requests
            .iter()
            .any(|r| claim::is_input(&r.split_whitespace().collect::<Vec<_>>()))
    );
}

fn initial_unsupported_claim(s: &mut Session<'_, Mock>) {
    s.ctl
        .claim_replies
        .push_back(err("unknown metadata key supervisor"));
    s.take_claim(&mut Alone);
    assert!(matches!(s.claim, claim::Claim::Unavailable { .. }));
}

#[test]
fn initial_unsupported_claims_keep_ordinary_supervisor_metadata_without_timer_authority() {
    let mut m = Mock::new(vec![question_screen()]);
    m.autopilot = Some("cruise");
    let mut s = virtual_session(&mut m, None);
    initial_unsupported_claim(&mut s);
    let point = box_point(&mut s);
    s.ctl
        .attention_replies
        .push_back(err("attention owners full"));
    s.escalate_point(&point, Some("a decision"), &mut Alone)
        .expect("legacy attempt");
    assert_eq!(s.ctl.attentions, 1);
    let claims = s.ctl.claims.len();
    s.retry_supervisor_attention(&point.screen, &auto(30), &mut Alone)
        .expect("deferred timer");
    assert_eq!(s.ctl.attentions, 1, "unheld timer cannot publish");
    assert_eq!(s.ctl.claims.len(), claims, "unheld timer cannot claim");
    s.write_attention_target().expect("ordinary legacy retry");
    assert_eq!(s.ctl.attentions, 2);
    assert!(s.ctl.attention.is_some());
    s.close_box(point.screen.seq, &mut Alone)
        .expect("legacy cleanup");
    assert_eq!(s.ctl.attention, None);
}

#[test]
fn initial_unsupported_claims_keep_ordinary_needs_metadata_without_timer_authority() {
    let mut m = Mock::new(vec![question_screen()]);
    m.autopilot = Some("cruise");
    let mut s = virtual_session(&mut m, None);
    initial_unsupported_claim(&mut s);
    let point = box_point(&mut s);
    s.turn_end.observe(
        &TurnEndReading {
            said_tail: Some("NEEDS YOU: the signing credential".to_string()),
            worked: Some(Duration::from_secs(60)),
            ..TurnEndReading::of(
                &s.reader(&point.screen.rows).read(&point.screen.rows, None),
                &point.screen.rows,
                ComposerRead::default(),
                None,
                None,
                None,
            )
        },
        s.fx.mono(),
    );
    s.ctl.needs_replies.push_back(err("attention owners full"));
    s.sync_needs(&point.screen, &auto(30), &mut Alone)
        .expect("legacy attempt");
    assert_eq!(s.ctl.needs_calls.len(), 1);
    let claims = s.ctl.claims.len();
    s.retry_needs(&point.screen, &auto(30), &mut Alone)
        .expect("deferred timer");
    assert_eq!(s.ctl.needs_calls.len(), 1, "unheld timer cannot publish");
    assert_eq!(s.ctl.claims.len(), claims, "unheld timer cannot claim");
    s.sync_needs(&point.screen, &auto(30), &mut Alone)
        .expect("ordinary legacy retry");
    assert!(s.ctl.needs_attention.is_some());
    s.needs_down(point.screen.seq, "resolved", &mut Alone)
        .expect("legacy cleanup");
    assert_eq!(s.ctl.needs_attention, None);
}

#[test]
fn a_previously_managed_claim_never_downgrades_to_unsupported_or_released_writes() {
    let mut m = Mock::new(vec![question_screen()]);
    m.autopilot = Some("cruise");
    let mut s = virtual_session(&mut m, None);
    active_claim(&mut s);
    let point = box_point(&mut s);
    s.escalate_point(&point, Some("a decision"), &mut Alone)
        .expect("managed SET");
    s.setup.claim_renew = Duration::from_millis(10);
    s.fx.sleep(Duration::from_millis(11));
    s.ctl
        .claim_replies
        .push_back(err("unknown metadata key supervisor"));
    s.call(&["meta"]).expect("ordinary renewal reply");
    assert!(matches!(s.claim, claim::Claim::Unavailable { .. }));
    s.ctl.supervisor_owner = Some("the-successor".to_string());
    s.ctl.attention = Some("the successor's badge".to_string());
    let unsets = count(s.ctl, "meta unset attention owner=supervisor");
    s.close_box(point.screen.seq, &mut Alone)
        .expect("deferred cleanup");
    assert_eq!(s.ctl.attention.as_deref(), Some("the successor's badge"));
    assert_eq!(
        count(s.ctl, "meta unset attention owner=supervisor"),
        unsets
    );
    assert!(s.attention_pending);
    s.release_claim();
    s.finish_supervisor_attention(point.screen.seq, &mut Alone)
        .expect("released cleanup considered");
    assert_eq!(s.ctl.attention.as_deref(), Some("the successor's badge"));
    assert_eq!(
        count(s.ctl, "meta unset attention owner=supervisor"),
        unsets
    );
}
