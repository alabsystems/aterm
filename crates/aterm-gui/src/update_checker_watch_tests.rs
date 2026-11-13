// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn a_failed_thread_start_retries_after_backoff_without_stall_confirmation() {
    for phase in [CheckerPhase::StartFailed, CheckerPhase::Stopped] {
        let mut state = CheckerWatchState::default();
        let failed = beat(9, phase, 100);
        let due = 100 + checker_watch::START_RETRY.as_secs();
        assert_eq!(next_look_in(&state, Some(failed), due - 1), Some(1));
        assert!(judge(&mut state, Some(failed), due - 1).is_none());
        assert_eq!(judge(&mut state, Some(failed), due).unwrap().generation, 9);
        assert!(judge(&mut state, Some(failed), due + 1).is_none());
        assert_eq!(next_look_in(&state, Some(failed), due + 1), None);
        let again = beat(10, phase, due);
        assert!(judge(&mut state, Some(again), due + 1).is_none());
        assert_eq!(
            judge(
                &mut state,
                Some(again),
                due + checker_watch::START_RETRY.as_secs()
            )
            .unwrap()
            .generation,
            10
        );
    }
}
use crate::update_words::LiveHealth as _;
use aterm_update::checker_watch::{
    CHECK_PHASE_BUDGET, CHECKER_LOCK_WAIT, LaneHolder, STALL_CONFIRM, STALL_SLACK,
    stall_threshold_secs,
};

fn beat(generation: u64, phase: CheckerPhase, at: u64) -> CheckerBeat {
    CheckerBeat {
        generation,
        phase,
        at_secs: at,
        max_interval_secs: 1_100,
        respawns: 0,
        workers: 1,
        worker_epoch: 0,
        deferrals: 0,
        settings_misses: 0,
        lane: None,
        checks: 0,
    }
}

const CONFIRM: u64 = STALL_CONFIRM.as_secs();

/// Judge `beat` twice, one confirmation apart: the second look's verdict.
fn confirmed(state: &mut CheckerWatchState, beat: CheckerBeat, now: u64) -> Option<CheckerStall> {
    assert_eq!(
        judge(state, Some(beat), now),
        None,
        "the first look only suspects"
    );
    judge(state, Some(beat), now + CONFIRM)
}

/// An unchanged stall is acted on once, and only a stall (here a checker parked in the
/// lock wait for four days).
#[test]
fn an_unchanged_stall_is_acted_on_once_and_a_fresh_beat_never() {
    let mut state = CheckerWatchState::default();
    assert_eq!(
        judge(&mut state, None, 1_000_000),
        None,
        "no checker, no look"
    );
    let fresh = beat(1, CheckerPhase::Waiting, 1_000);
    assert_eq!(judge(&mut state, Some(fresh), 1_000 + 60), None);
    let parked = beat(1, CheckerPhase::LockWait, 1_000);
    let days = 1_000 + 4 * 86_400;
    assert_eq!(
        confirmed(&mut state, parked, days),
        Some(CheckerStall {
            generation: 1,
            for_secs: 4 * 86_400 + CONFIRM,
            phase: CheckerPhase::LockWait,
            holds_lane: false,
            checks: 0,
        })
    );
    assert_eq!(
        judge(&mut state, Some(parked), days + CONFIRM + 60),
        None,
        "the same stall is acted on once"
    );
    let replacement = beat(2, CheckerPhase::Checking, days);
    let later = days + stall_threshold_secs(CheckerPhase::Checking, 1_100) + 1;
    assert!(confirmed(&mut state, replacement, later).is_some());
}

#[test]
fn a_returned_worker_slot_rearms_a_confirmed_stall_once_without_another_delay() {
    let mut state = CheckerWatchState::default();
    let mut parked = beat(4, CheckerPhase::Checking, 1_000);
    parked.workers = 4;
    let now = 1_000 + 4 * 86_400;
    assert!(confirmed(&mut state, parked, now).is_some());
    assert_eq!(next_look_in(&state, Some(parked), now + CONFIRM), None);
    let old = parked;
    parked.workers -= 1;
    parked.worker_epoch += 1;
    assert_eq!(next_look_in(&state, Some(parked), now + CONFIRM), Some(0));
    assert!(judge(&mut state, Some(parked), now + CONFIRM).is_some());
    assert!(judge(&mut state, Some(parked), now + CONFIRM).is_none());
    assert!(
        judge(&mut state, Some(old), now + CONFIRM).is_none(),
        "an older snapshot cannot rearm it"
    );
    assert_eq!(next_look_in(&state, Some(old), now + CONFIRM), None);
    parked.worker_epoch += 1;
    parked.at_secs = now + CONFIRM;
    assert!(
        judge(&mut state, Some(parked), now + CONFIRM).is_none(),
        "a fresh worker does not need replacing"
    );
    let stale = parked.at_secs + stall_threshold_secs(parked.phase, parked.max_interval_secs) + 1;
    assert!(
        judge(&mut state, Some(parked), stale).is_none(),
        "a different heartbeat needs its own confirmation"
    );
    assert_eq!(next_look_in(&state, Some(parked), stale), Some(CONFIRM));
    assert!(judge(&mut state, Some(parked), stale + CONFIRM).is_some());
}

/// A process that was stopped is not a stalled checker: the first look after a continue
/// only suspects, the checker stamps, and nothing happens.
#[test]
fn a_stamp_that_moves_before_the_second_look_is_no_stall() {
    let mut state = CheckerWatchState::default();
    let threshold = stall_threshold_secs(CheckerPhase::Waiting, 1_100);
    let before_the_stop = beat(1, CheckerPhase::Waiting, 1_000);
    let continued = 1_000 + threshold + 3_600;
    assert_eq!(
        judge(&mut state, Some(before_the_stop), continued),
        None,
        "the first look after the process was continued only suspects"
    );
    assert_eq!(
        next_look_in(&state, Some(before_the_stop), continued),
        Some(CONFIRM),
        "and folds its second look"
    );
    assert_eq!(
        judge(&mut state, Some(before_the_stop), continued + CONFIRM - 1),
        None,
        "not before it is due"
    );
    let restamped = beat(1, CheckerPhase::Settings, continued + 25);
    assert_eq!(
        judge(&mut state, Some(restamped), continued + CONFIRM),
        None
    );
    assert_eq!(state.suspect, None, "the suspicion is dropped");
    assert_eq!(state.acted_on, None);
    let stuck = beat(1, CheckerPhase::Checking, 0);
    let stale = stall_threshold_secs(CheckerPhase::Checking, 1_100) + 1;
    assert!(confirmed(&mut state, stuck, stale).is_some());
}

/// The wake is folded from the look's own state: a stale stamp no look has seen folds an
/// immediate wake, a suspicion its second look, and only an acted-on stall folds nothing.
#[test]
fn a_stamp_gone_stale_between_the_look_and_the_fold_still_folds_a_wake() {
    let mut state = CheckerWatchState::default();
    let threshold = stall_threshold_secs(CheckerPhase::Waiting, 1_100);
    let b = beat(1, CheckerPhase::Waiting, 0);
    assert_eq!(judge(&mut state, Some(b), threshold), None);
    assert_eq!(
        next_look_in(&state, Some(b), threshold + 1),
        Some(0),
        "a look is owed now"
    );
    assert_eq!(next_look_in(&state, None, 10), None, "no checker, no wake");
    assert_eq!(
        next_look_in(&state, Some(b), 10),
        Some(threshold - 10 + 1),
        "a fresh stamp folds the instant it would go stale"
    );
    let stall = confirmed(&mut state, b, threshold + 1);
    assert!(stall.is_some());
    assert_eq!(
        next_look_in(&state, Some(b), threshold + 1 + CONFIRM),
        None,
        "a stall acted on folds nothing"
    );
}

/// The stall threshold is the phase's: a lock wait is called out in minutes,
/// a check only after twice its whole budget.
#[test]
fn a_lock_wait_stalls_in_minutes_and_a_check_only_past_its_budget() {
    let mut state = CheckerWatchState::default();
    let lock = 2 * CHECKER_LOCK_WAIT.as_secs() + STALL_SLACK.as_secs();
    assert!(judge(&mut state, Some(beat(1, CheckerPhase::LockWait, 0)), lock).is_none());
    assert!(state.suspect.is_none(), "not stale yet");
    assert!(confirmed(&mut state, beat(1, CheckerPhase::LockWait, 0), lock + 1).is_some());
    let mut state = CheckerWatchState::default();
    assert!(
        judge(
            &mut state,
            Some(beat(1, CheckerPhase::Checking, 0)),
            2 * CHECK_PHASE_BUDGET.as_secs()
        )
        .is_none()
            && state.suspect.is_none(),
        "a long check is not a stall"
    );
}

/// The status tokens appear only in the state they name — and a stall inside
/// the lane stays named while a replacement is blocked behind it.
#[test]
fn the_status_tokens_are_absent_on_a_healthy_checker() {
    assert_eq!(checker_status_tokens(None, 10), "");
    assert_eq!(
        checker_status_tokens(Some(beat(1, CheckerPhase::Waiting, 100)), 160),
        "",
        "a healthy line stays byte-identical"
    );
    let mut stalled = beat(2, CheckerPhase::LockWait, 0);
    stalled.respawns = 1;
    stalled.deferrals = 7;
    assert_eq!(
        checker_status_tokens(Some(stalled), 3_600),
        " checker_stalled=3600:lock-wait checker_respawns=1 checker_deferred=7"
    );
    let mut blocked = beat(2, CheckerPhase::Waiting, 5_000);
    blocked.respawns = 1;
    blocked.lane = Some(LaneHolder {
        generation: 1,
        phase: CheckerPhase::Checking,
        at_secs: 1_000,
    });
    assert_eq!(
        checker_status_tokens(Some(blocked), 5_060),
        " checker_stalled=4060:checking checker_respawns=1"
    );
}

/// A stall posts its own warning once per launch with its own sentence, and does not latch
/// the ledger's "can't check for updates": a later network or manifest failure is still
/// said.
#[test]
fn a_stall_raises_its_own_warning_once_and_never_silences_the_ledgers() {
    let mut app = App::headless_for_test();
    let stall = CheckerStall {
        generation: 1,
        for_secs: 3_000,
        phase: CheckerPhase::LockWait,
        holds_lane: false,
        checks: 0,
    };
    app.announce_update_checker_stall(stall, Some(2));
    let live = app
        .messages
        .live_health()
        .expect("the health warning is up");
    assert_eq!(live.msg.title, crate::update_words::CHECKER_STALLED_TITLE);
    let before = app.messages.live_health().map(|l| l.id);
    app.announce_update_checker_stall(
        CheckerStall {
            generation: 2,
            ..stall
        },
        None,
    );
    assert_eq!(
        app.messages.live_health().map(|l| l.id),
        before,
        "announced once per launch"
    );
    assert!(
        app.note_update_health(
            aterm_update::health_failing_title("network"),
            "update checks have failed 5 times in a row"
        ),
        "the ledger's own warning is still owed its row"
    );
}

#[test]
fn a_checker_stall_is_recorded_without_a_banner() {
    let mut app = App::headless_for_test();
    app.announce_update_checker_stall(
        CheckerStall {
            generation: 1,
            for_secs: 3_000,
            phase: CheckerPhase::LockWait,
            holds_lane: true,
            checks: 0,
        },
        Some(2),
    );
    assert!(!app.messages.log().is_empty());
}

/// The watchdog's heals end the digest's day too: announcing a stall or a held lock claims
/// a digest line (`update.review.stalled`, `update.review.lock-held`), and both heals go
/// through `App::heal_update_health_where`, which ends the claim.
#[test]
fn a_watchdog_heal_withdraws_its_digest_claim() {
    let mut app = App::headless_for_test();
    assert!(app.ripening_deadline().is_none());
    app.announce_update_checker_stall(
        CheckerStall {
            generation: 1,
            for_secs: 4_000,
            phase: CheckerPhase::Checking,
            holds_lane: true,
            checks: 3,
        },
        Some(2),
    );
    app.announce_checker_lock_held(10);
    assert!(app.ripening_deadline().is_some(), "both claim the day");
    app.heal_update_checker_stall();
    assert!(
        app.ripening_deadline().is_some(),
        "NEGATIVE: the lock-held claim stands"
    );
    app.heal_checker_lock_held();
    assert!(
        app.ripening_deadline().is_none(),
        "both heals withdrew their claims"
    );
}

/// The stall warning heals on the first check completed after it.
#[test]
fn the_stall_warning_heals_when_a_check_completes_after_it() {
    let mut app = App::headless_for_test();
    let mut stuck = beat(1, CheckerPhase::Settings, 0);
    stuck.checks = 4;
    let stale = stall_threshold_secs(CheckerPhase::Settings, 1_100) + 1;
    app.look_at_update_checker(Some(stuck), stale);
    assert!(
        app.messages.live_health().is_none(),
        "one stale look only suspects"
    );
    app.look_at_update_checker(Some(stuck), stale + CONFIRM);
    assert!(
        app.messages
            .live_health()
            .is_some_and(|live| live.msg.title == crate::update_words::CHECKER_STALLED_TITLE),
        "the confirmed stall is announced"
    );
    assert!(
        app.ripening_deadline().is_some(),
        "its digest line is claimed"
    );
    let mut replacement = beat(2, CheckerPhase::Waiting, stale + CONFIRM + 10);
    replacement.checks = 4;
    replacement.respawns = 1;
    app.look_at_update_checker(Some(replacement), stale + CONFIRM + 20);
    assert!(
        app.messages.live_health().is_some(),
        "no check has completed since: the warning stands"
    );
    replacement.checks = 5;
    app.look_at_update_checker(Some(replacement), stale + CONFIRM + 700);
    assert!(
        app.messages.live_health().is_none(),
        "a completed check healed it"
    );
    assert!(
        !app.update_health_latched
            .contains(&crate::update_words::HealthKind::Stalled),
        "and its latch: a later stall is announced again"
    );
    assert!(
        app.ripening_deadline().is_none(),
        "and its digest claim: no line a day after it healed"
    );
}

/// The ledger's healing is no proof the stall is over: with generation 1 stalled holding
/// the lane and its replacement blocked, a sibling's good check heals the shared ledger and
/// the replacement reports the recovery, which takes the check warning down but not the
/// stall's.
#[test]
fn a_blocked_replacement_over_a_healed_ledger_leaves_the_stall_up() {
    use crate::update_words::HealthKind;
    let stalled = HealthKind::Stalled;
    let recorded = |app: &App| {
        app.messages
            .log()
            .records()
            .filter(|r| r.title == "aterm updates work again")
            .map(|r| r.detail.join(" "))
            .collect::<Vec<_>>()
    };
    let body = "20 failed checks in a row since 2026-09-14T22:04:36Z: the reason.";
    for class in ["manifest", "pipeline", "apply", ""] {
        let mut app = App::headless_for_test();
        assert!(app.note_update_health(aterm_update::health_failing_title("manifest"), body));
        app.announce_update_checker_stall(
            CheckerStall {
                generation: 1,
                for_secs: 4_000,
                phase: CheckerPhase::Checking,
                holds_lane: true,
                checks: 5,
            },
            Some(2),
        );
        assert_eq!(app.live_update_health(|k| k == stalled).len(), 1);
        let mut blocked = beat(2, CheckerPhase::Waiting, 10_000);
        blocked.checks = 5;
        blocked.respawns = 1;
        blocked.lane = Some(LaneHolder {
            generation: 1,
            phase: CheckerPhase::Checking,
            at_secs: 5_000,
        });
        app.look_at_update_checker(Some(blocked), 10_010);
        assert!(!app.note_update_health(aterm_update::HEALTH_RECOVERED_TITLE, class));
        assert!(
            app.live_update_health(|k| k == HealthKind::Check)
                .is_empty(),
            "the ledger's own warning heals ({class:?})"
        );
        assert!(
            app.update_health_latched.contains(&stalled),
            "the stall's latch stands ({class:?})"
        );
        assert_eq!(
            app.live_update_health(|k| k == stalled).len(),
            1,
            "the stall's row stands ({class:?})"
        );
        assert_eq!(
            recorded(&app),
            vec!["after \"Couldn't check for updates\", since Sep 14".to_string()],
            "only the check warning's healing is on record ({class:?})"
        );
        blocked.at_secs = 10_600;
        app.look_at_update_checker(Some(blocked), 10_610);
        assert!(app.update_health_latched.contains(&stalled));
        blocked.lane = None;
        blocked.checks = 6;
        blocked.at_secs = 11_200;
        app.look_at_update_checker(Some(blocked), 11_210);
        assert!(!app.update_health_latched.contains(&stalled));
        assert!(app.live_update_health(|k| k == stalled).is_empty());
        assert_eq!(recorded(&app).len(), 2, "the stall's healing, once");
    }
    let mut app = App::headless_for_test();
    app.announce_update_checker_stall(
        CheckerStall {
            generation: 1,
            for_secs: 4_000,
            phase: CheckerPhase::Checking,
            holds_lane: true,
            checks: 5,
        },
        Some(2),
    );
    app.heal_update_health(crate::messages_host::HealthProof::Checked);
    assert!(!app.update_health_latched.contains(&stalled));
    assert!(app.live_update_health(|k| k == stalled).is_empty());
}

/// A sibling holding `checker.lock` without progress is said once per streak, when the
/// published deferrals reach `CHECKER_UNGATED_AFTER` (where the loop starts checking
/// without the lock), and the warning heals when the streak ends, not when a lock-free
/// check completes.
#[test]
fn a_sibling_holding_the_checker_lock_is_said_once_and_healed_when_it_lets_go() {
    use aterm_update::checker_watch::CHECKER_UNGATED_AFTER;
    let mut app = App::headless_for_test();
    let mut waiting = beat(1, CheckerPhase::Waiting, 100);
    let fresh = CheckerWatchState::default();
    assert!(
        next_look_in(&fresh, Some(waiting), 100).is_some_and(|secs| secs > DEFERRAL_LOOK_SECS),
        "a healthy beat folds no minute-look"
    );
    for deferrals in 1..CHECKER_UNGATED_AFTER {
        waiting.deferrals = deferrals;
        app.look_at_update_checker(Some(waiting), 110);
        assert!(
            app.messages.live_health().is_none(),
            "below the bound the sibling may just be checking: {deferrals}"
        );
        assert_eq!(
            next_look_in(&app.update_checker_watch, Some(waiting), 110),
            Some(DEFERRAL_LOOK_SECS),
            "a standing streak is looked at within a minute"
        );
    }
    waiting.deferrals = CHECKER_UNGATED_AFTER;
    app.look_at_update_checker(Some(waiting), 120);
    let live = app
        .messages
        .live_health()
        .expect("the stuck sibling is said");
    assert_eq!(live.msg.title, crate::update_words::CHECKER_LOCK_HELD_TITLE);
    assert_eq!(
        live.msg.detail.first().map(String::as_str),
        Some(crate::update_words::checker_lock_held_body(CHECKER_UNGATED_AFTER).as_str())
    );
    waiting.deferrals += 1;
    waiting.checks += 2;
    app.look_at_update_checker(Some(waiting), 700);
    assert!(
        app.messages.live_health().is_some(),
        "a check without the lock does not heal a holder that is still stuck"
    );
    assert_eq!(
        app.messages
            .live_rows()
            .filter(|l| crate::update_words::HealthKind::of_row(&l.msg).is_some())
            .count(),
        1,
        "said once per streak"
    );
    waiting.deferrals = 0;
    app.look_at_update_checker(Some(waiting), 1_300);
    assert!(app.messages.live_health().is_none(), "healed");
    assert!(
        !app.update_health_latched
            .contains(&crate::update_words::HealthKind::LockHeld),
        "and its latch: the next streak is said again"
    );
    assert!(app.ripening_deadline().is_none(), "and its digest claim");
    waiting.deferrals = CHECKER_UNGATED_AFTER;
    app.look_at_update_checker(Some(waiting), 2_000);
    assert!(app.messages.live_health().is_some(), "a new streak is said");
}

/// The live rows standing for the lock-held warning, by its words.
fn lock_held_rows(app: &App) -> usize {
    app.messages
        .live_rows()
        .filter(|l| crate::update_words::HealthKind::of_row(&l.msg).is_some())
        .filter(|l| {
            l.msg
                .detail
                .first()
                .is_some_and(|d| d.contains("another aterm has held the update check's lock"))
        })
        .count()
}

/// Records of "aterm updates work again".
fn healed_records(app: &App) -> usize {
    app.messages
        .log()
        .records()
        .filter(|r| r.title == "aterm updates work again")
        .count()
}

/// A replacement checker starts at zero deferrals, which is no proof the sibling let go:
/// with the lock-held warning up and generation 1 retired, the replacement's zero leaves
/// the warning standing.
#[test]
fn a_replacement_at_zero_deferrals_is_no_proof_the_sibling_let_go() {
    use aterm_update::checker_watch::CHECKER_UNGATED_AFTER;
    let mut app = App::headless_for_test();
    let mut waiting = beat(1, CheckerPhase::Waiting, 100);
    waiting.deferrals = CHECKER_UNGATED_AFTER;
    app.look_at_update_checker(Some(waiting), 110);
    assert_eq!(lock_held_rows(&app), 1, "the stuck sibling is said");
    app.announce_update_checker_stall(
        CheckerStall {
            generation: 1,
            for_secs: 3_000,
            phase: CheckerPhase::Checking,
            holds_lane: true,
            checks: 0,
        },
        Some(2),
    );
    let mut replacement = beat(2, CheckerPhase::Starting, 200);
    app.look_at_update_checker(Some(replacement), 260);
    assert_eq!(
        lock_held_rows(&app),
        1,
        "the sibling still holds the lock: the warning stands"
    );
    assert_eq!(
        healed_records(&app),
        0,
        "and nothing says updates work again"
    );
    replacement.phase = CheckerPhase::Waiting;
    replacement.checks = 1;
    app.look_at_update_checker(Some(replacement), 320);
    assert_eq!(lock_held_rows(&app), 0, "healed once a check proves it");
}

/// The lock-held warning is never swallowed by a stall warning already up: each kind has
/// its own latch.
#[test]
fn a_stall_warning_up_first_does_not_swallow_the_lock_held_warning() {
    use aterm_update::checker_watch::CHECKER_UNGATED_AFTER;
    let mut app = App::headless_for_test();
    app.announce_update_checker_stall(
        CheckerStall {
            generation: 1,
            for_secs: 3_000,
            phase: CheckerPhase::Settings,
            holds_lane: false,
            checks: 4,
        },
        Some(2),
    );
    let mut b = beat(2, CheckerPhase::Waiting, 100);
    b.checks = 4;
    b.deferrals = CHECKER_UNGATED_AFTER;
    app.look_at_update_checker(Some(b), 110);
    b.checks = 5;
    b.deferrals += 1;
    app.look_at_update_checker(Some(b), 700);
    assert_eq!(
        lock_held_rows(&app),
        1,
        "the stuck sibling is still holding the lock and must be said"
    );
}

/// A download is no proof the sibling let go: download progress leaves the lock-held
/// warning up.
#[test]
fn a_download_does_not_heal_the_lock_held_warning() {
    use aterm_update::checker_watch::CHECKER_UNGATED_AFTER;
    let mut app = App::headless_for_test();
    let mut waiting = beat(1, CheckerPhase::Waiting, 100);
    waiting.deferrals = CHECKER_UNGATED_AFTER;
    app.look_at_update_checker(Some(waiting), 120);
    assert_eq!(lock_held_rows(&app), 1);
    app.note_update_progress(&aterm_update::Progress::Downloading {
        version: "9.9.9".into(),
        bytes_done: 1,
        bytes_total: 10,
    });
    assert_eq!(lock_held_rows(&app), 1, "the sibling is still stuck");
    assert_eq!(healed_records(&app), 0, "{:?}", app.messages.live_health());
}

/// A window with no checker (every headless App, every test binary) looks
/// and folds nothing.
#[test]
fn no_checker_means_no_look_and_no_wake() {
    let mut app = App::headless_for_test();
    app.watch_update_checker();
    assert!(app.update_checker_deadline().is_none());
    assert!(app.messages.live_health().is_none());
}
