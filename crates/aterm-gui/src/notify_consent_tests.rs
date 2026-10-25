// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A pending desktop authorization must not prolong a consent condition.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

use super::{NotifyMsg, Route, delivers, way_for};
use crate::consent_observer::{
    AttentionEvent, AttentionGate, ObserverEvent, ObserverMode, ObserverState,
    PENDING_PROMPT_THRESHOLD, TccEvent,
};
use crate::notice_center::{Center, NoticeCenter};

type Answer = Box<dyn FnOnce() + Send>;
static ASK: Mutex<Option<mpsc::SyncSender<Answer>>> = Mutex::new(None);
static AUTHORIZATION: Mutex<()> = Mutex::new(());
static GRANTED: AtomicBool = AtomicBool::new(false);
const HANG: Duration = Duration::from_secs(60);

fn answer() -> Center {
    if GRANTED.load(Ordering::Acquire) {
        Center::Granted
    } else {
        Center::Unasked
    }
}

fn ask(then: Answer) {
    ASK.lock().unwrap().as_ref().unwrap().send(then).unwrap();
}

fn authorize_after(queued: NotifyMsg, change: impl FnOnce()) -> Route {
    let _authorization = AUTHORIZATION.lock().unwrap();
    let (asked, authorization) = mpsc::sync_channel(1);
    *ASK.lock().unwrap() = Some(asked);
    GRANTED.store(false, Ordering::Release);
    let center = NoticeCenter::stand_in(answer, ask);
    let (finished, result) = mpsc::sync_channel(1);
    let delivery = std::thread::spawn(move || {
        let route = way_for(center, true, true, &AtomicBool::new(false), || {
            delivers(&queued, &HashSet::new(), true)
        });
        finished.send(route).unwrap();
    });
    let grant = authorization
        .recv_timeout(HANG)
        .expect("the queued notice is waiting for desktop authorization");
    assert_eq!(center.answer(), Center::Unasked);
    change();
    GRANTED.store(true, Ordering::Release);
    grant();
    let route = result.recv_timeout(HANG).expect("authorization completed");
    delivery.join().unwrap();
    *ASK.lock().unwrap() = None;
    route
}

#[test]
fn ended_consent_conditions_cannot_outlive_held_notification_authorization() {
    #[derive(Clone, Copy, Debug)]
    enum End {
        PromptAnswered,
        ObserverStopped,
        PromptRetired,
        DenialRecovered,
        DenialRetired,
        UnattributedDenialRecovered,
    }

    let mut stale = Vec::new();
    for end in [
        End::PromptAnswered,
        End::ObserverStopped,
        End::PromptRetired,
        End::DenialRecovered,
        End::DenialRetired,
        End::UnattributedDenialRecovered,
    ] {
        let now = Instant::now();
        let mut observer = ObserverState::inert();
        observer.feed_for_test(ObserverEvent::Started, now);
        for (id, pid) in [("prompt-a", 710), ("prompt-b", 810)] {
            for event in [
                TccEvent::Ctx {
                    msg_id: id.into(),
                    service: "kTCCServiceCamera".into(),
                    preflight: Some(false),
                },
                TccEvent::Attribution {
                    msg_id: id.into(),
                    accessing_pid: Some(pid),
                    requesting_pid: None,
                },
            ] {
                observer.feed_for_test(ObserverEvent::Line(event), now);
            }
        }
        let later = now + PENDING_PROMPT_THRESHOLD;
        let prompts = observer.take_new_verdicts(
            later,
            1,
            || vec![(7, 71), (8, 81)],
            |pid| match pid {
                710 => Some(71),
                810 => Some(81),
                _ => None,
            },
        );
        assert_eq!(prompts.len(), 2, "{end:?}: both actual dialogs announced");
        assert!(observer.marks_session(7) && observer.marks_session(8));
        let prompt_a = prompts.iter().find(|prompt| prompt.session == 7).unwrap();
        let prompt_b = prompts.iter().find(|prompt| prompt.session == 8).unwrap();
        let live_b = NotifyMsg::consent(&prompt_b.notice());
        let mut gate = AttentionGate::new();
        let unattributed = matches!(end, End::UnattributedDenialRecovered);
        let notice = if matches!(
            end,
            End::PromptAnswered | End::ObserverStopped | End::PromptRetired
        ) {
            prompt_a.notice()
        } else {
            let session = (!unattributed).then_some(7);
            let outcome = gate
                .note(AttentionEvent::ProtectedEperm { session })
                .for_mode(ObserverMode::Full);
            assert_eq!(outcome.raise_tab, session);
            if let Some(session) = session {
                assert!(gate.mark_session(session, now));
            }
            let notice = outcome.notice.unwrap();
            assert_eq!(notice.session, session.unwrap_or(u64::MAX));
            notice
        };
        let queued = NotifyMsg::consent(&notice);
        let same_intent = NotifyMsg::consent(&notice.clone());
        let background = HashSet::new();
        let program = NotifyMsg::program(7, None, "independent program event".into());
        assert!(delivers(&queued, &background, true), "{end:?}: live intent");
        assert!(delivers(&live_b, &background, true));
        assert!(!delivers(&queued, &HashSet::from([queued.session]), true));
        assert!(!delivers(&queued, &background, false));
        assert!(delivers(&program, &background, false));

        let route = authorize_after(queued, || {
            match end {
                End::PromptAnswered => observer.feed_for_test(
                    ObserverEvent::Line(TccEvent::Result {
                        msg_id: "prompt-a".into(),
                    }),
                    later,
                ),
                End::ObserverStopped => observer.stop(),
                End::PromptRetired => observer.retire_session(7),
                End::DenialRecovered => {
                    let newer = now + Duration::from_secs(2);
                    let outcome = gate
                        .note(AttentionEvent::ProtectedEperm { session: Some(8) })
                        .for_mode(ObserverMode::Full);
                    assert!(outcome.notice.is_none(), "the instance budget stays spent");
                    assert!(gate.mark_session(8, newer));
                    assert!(gate.recover(now + Duration::from_secs(1)));
                    assert!(!gate.marks_session(7) && gate.marks_session(8));
                    assert!(
                        delivers(&same_intent, &background, true),
                        "an older grant cannot end the FDA action a newer denial still needs"
                    );
                    assert!(gate.recover(newer));
                    assert!(!gate.has_denials());
                }
                End::DenialRetired => {
                    gate.retire_session(7);
                    assert!(!gate.has_denials());
                }
                End::UnattributedDenialRecovered => {
                    gate.retire_session(7);
                    assert!(!gate.marks_session(7));
                    assert!(
                        delivers(&same_intent, &background, true),
                        "retiring an unrelated session cannot end an unattributed denial"
                    );
                    gate.recover(Instant::now());
                }
            }
            if matches!(end, End::PromptAnswered | End::PromptRetired) {
                assert!(!observer.marks_session(7));
            }
            if matches!(end, End::ObserverStopped) {
                assert!(!observer.marks_session(7) && !observer.marks_session(8));
            } else {
                assert!(observer.marks_session(8));
                assert!(
                    delivers(&live_b, &background, true),
                    "{end:?}: B still waits"
                );
            }
            assert!(delivers(&program, &background, false));
        });
        if route != Route::Lapsed {
            stale.push((end, route));
        }
    }
    assert!(
        stale.is_empty(),
        "ended consent conditions still deliver: {stale:?}"
    );
}

#[test]
fn app_grant_recovery_cancels_an_unattributed_denial_without_rearming_its_notice() {
    use crate::App;
    use crate::consent_state::ConsentState;
    use aterm_containment::consent::{FdaProbe, FdaState, ProbeLabel};

    let mut app = App::headless_for_test();
    app.config.privacy = Some(crate::app_config::PrivacyConfig {
        observer: Some(true),
        notice: Some(false),
        ..Default::default()
    });
    app.config.desktop_alerts = Some(true);
    let (notices, posted) = mpsc::sync_channel(4);
    app.session_factory.notify_tx = notices;
    app.note_consent_attention(AttentionEvent::ProtectedEperm { session: None });
    let queued = posted.try_recv().expect("the actual App queued its notice");
    assert_eq!(queued.session, u64::MAX);
    assert!(delivers(&queued, &HashSet::from([0]), true));
    assert!(!app.consent_attention.marks_session(0));
    app.on_consent_probe_ready(Instant::now());
    assert!(app.consent_attention.has_denials(), "unknown is no grant");
    assert!(delivers(&queued, &HashSet::new(), true));
    app.note_consent_attention(AttentionEvent::ProtectedEperm { session: Some(0) });
    assert!(app.consent_attention.marks_session(0));
    app.consent_attention.retire_session(0);
    assert!(
        app.consent_attention.has_denials(),
        "the unattributed refusal survives a later session refusal and retirement"
    );
    assert!(delivers(&queued, &HashSet::new(), true));

    let route = authorize_after(queued, || {
        app.consent = ConsentState::with_cached_probe_for_test(FdaProbe {
            state: FdaState::Granted,
            label: ProbeLabel::OpenOk,
        });
        assert!(app.consent.window_reads_own_grant());
        app.on_consent_probe_ready(Instant::now());
        assert!(
            !app.consent_attention.has_denials(),
            "App must admit a cached grant for a denial with no session"
        );
        app.note_consent_attention(AttentionEvent::ProtectedEperm { session: None });
        assert!(app.consent_attention.has_denials());
        assert!(
            posted.try_recv().is_err(),
            "ending the old condition cannot rearm the per-instance notice budget"
        );
    });
    assert_eq!(
        route,
        Route::Lapsed,
        "a later denial cannot revive the old ticket"
    );
}
