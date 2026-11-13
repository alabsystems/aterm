// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_agent::supervise::policy::turn_end::PermissionTo;

fn click(tab: &str) -> Click {
    Click {
        tab: tab.to_owned(),
        conversation: "c-1".to_owned(),
        to: PermissionTo::Bypass,
        at: 1,
    }
}

/// A TAKE-BACK PAST THE FENCE LANDS UNLESS THE SIGNAL WENT (design §2: a click on `◐` "takes
/// it back, until the signal goes"): once the lane's look read the ask standing, before the
/// hand, a click on it still offered as one to take back marks it, the window draws and holds
/// it no more, the fence's read just before the signal finds it gone, and the look's answer
/// that it waits ends it rather than leaving it to signal at the next rest. NEGATIVE CONTROLS:
/// a signal already past the fence's last read keeps the ask, and nothing takes it back after;
/// with no look under way the first click takes it back at once, and a look for it moves
/// nothing.
#[test]
fn a_take_back_past_the_fence_lands_unless_the_signal_went() {
    let clicks = Clicks::default();
    let now = Instant::now();
    let id = clicks.stand(click("s-a"), false);
    assert!(clicks.stands("s-a", id), "the look's read before the hand");
    assert_eq!(
        clicks.shown("s-a"),
        Some(ClickShown {
            when: Some("when it can")
        }),
        "past the fence, unsignalled: still offered to take back"
    );
    assert!(
        clicks.take_back("s-a").is_some(),
        "taken back past the fence"
    );
    assert_eq!(clicks.shown("s-a"), None, "drawn as taken back");
    assert!(clicks.get("s-a").is_none());
    assert!(!clicks.holds("s-a", now), "it holds nothing");
    assert!(
        !clicks.stands("s-a", id),
        "the read before the signal finds it gone"
    );
    assert_eq!(
        clicks.note("s-a", id, &Look::Waits(Wait::Other("changed".into())), now),
        Noted::Changed
    );
    assert!(
        clicks.lane_next("s-a").is_none(),
        "the look's wait ended it, and its lane with it"
    );

    let id = clicks.stand(click("s-c"), false);
    assert!(clicks.stands("s-c", id));
    assert!(
        clicks.take_back("s-c").is_some(),
        "after the fence's last read"
    );
    assert_eq!(
        clicks.note("s-c", id, &Look::InFlight("signalled".into()), now),
        Noted::Changed
    );
    assert!(
        clicks.get("s-c").is_some_and(|s| s.signalled),
        "control: the signal went first"
    );
    assert!(clicks.take_back("s-c").is_none(), "control: signalled");

    let id = clicks.stand(click("s-b"), false);
    assert!(clicks.take_back("s-b").is_some(), "no look under way");
    assert!(!clicks.stands("s-b", id));
    assert_eq!(
        clicks.note("s-b", id, &Look::InFlight("x".into()), now),
        Noted::Stale,
        "a look for an ask taken back moves nothing"
    );
}

/// ONE RESTART A CLICK (the model's `OneRestartPerClick`): once its restart was signalled, a
/// later look that waits (its record superseded by another restart's, which it waits behind)
/// and then reaches the restart again reads the fence false, so it ends with nothing signalled
/// a second time. NEGATIVE CONTROL: an ask not yet signalled reads it standing after a wait.
#[test]
fn a_signalled_click_never_passes_the_fence_again() {
    let clicks = Clicks::default();
    let now = Instant::now();
    let id = clicks.stand(click("s-a"), false);
    assert!(clicks.stands("s-a", id));
    assert_eq!(
        clicks.note("s-a", id, &Look::InFlight("signalled".into()), now),
        Noted::Changed
    );
    assert_eq!(
        clicks.note("s-a", id, &Look::Waits(Wait::Restart), now),
        Noted::Changed
    );
    assert!(
        !clicks.stands("s-a", id),
        "signalled once: no second signal"
    );
    let id = clicks.stand(click("s-b"), false);
    assert!(clicks.stands("s-b", id));
    clicks.note("s-b", id, &Look::Waits(Wait::Turn), now);
    assert!(clicks.stands("s-b", id), "control: not signalled yet");
}

/// THE HOLD (design Appendix, "Hold"): it starts at the first rest asked, a box or a held tab
/// starts it again, a draft does not, a turn clears it while it has time left (so a later
/// rest starts it again) and leaves it out once it ran out, and a wall lets it go while it
/// stands, the turn after it holding again. NEGATIVE CONTROL: a click taken back holds nothing.
#[test]
fn a_standing_click_holds_the_loops_act_for_its_own_bound() {
    let clicks = Clicks::default();
    let id = clicks.stand(click("s-h"), false);
    let now = Instant::now();
    assert!(clicks.holds("s-h", now), "from the first rest");
    clicks.run_hold_out("s-h");
    assert!(!clicks.holds("s-h", Instant::now()), "its bound");
    let id2 = clicks.stand(click("s-h"), false);
    assert!(id2 > id, "a new ask");
    assert!(clicks.holds("s-h", Instant::now()));
    clicks.run_hold_out("s-h");
    clicks.note("s-h", id2, &Look::Waits(Wait::Box), Instant::now());
    assert!(clicks.holds("s-h", Instant::now()), "a box starts it again");
    clicks.run_hold_out("s-h");
    clicks.note("s-h", id2, &Look::Waits(Wait::Draft), Instant::now());
    assert!(!clicks.holds("s-h", Instant::now()), "a draft does not");
    let gen3 = clicks.stand(click("s-h"), false);
    let rest = Instant::now();
    assert!(clicks.holds("s-h", rest));
    clicks.note("s-h", gen3, &Look::Waits(Wait::Turn), rest);
    assert!(
        clicks.holds("s-h", rest + CLICK_HOLD * 2),
        "a turn with time left cleared it: a later rest starts it again"
    );
    clicks.note("s-h", gen3, &Look::Waits(Wait::Held), Instant::now());
    clicks.run_hold_out("s-h");
    clicks.note("s-h", gen3, &Look::Waits(Wait::Turn), Instant::now());
    assert!(!clicks.holds("s-h", Instant::now()), "run out, out");
    clicks.note("s-h", gen3, &Look::Waits(Wait::Box), Instant::now());
    assert!(!clicks.holds("s-h", Instant::now()), "and out for this ask");
    let gen4 = clicks.stand(click("s-h"), false);
    clicks.note("s-h", gen4, &Look::Waits(Wait::Limit), Instant::now());
    assert!(!clicks.holds("s-h", Instant::now()), "a wall's own wait");
    clicks.note("s-h", gen4, &Look::Waits(Wait::Turn), Instant::now());
    assert!(
        clicks.holds("s-h", Instant::now()),
        "the turn after the wall: the next rest holds again"
    );
    clicks.stand(click("s-h"), false);
    assert!(clicks.take_back("s-h").is_some());
    assert!(!clicks.holds("s-h", Instant::now()), "nothing stands");
}

fn default_of(mode: Resolved) -> PermissionDefault {
    PermissionDefault {
        mode,
        bypass_disabled: None,
    }
}

/// THE WINDOW READS A DEFAULT THE RESOLVER READ: a tab's default is due until it is read and
/// again once it has stood the recheck; a read is held for the agent it was read for, and a
/// relaunched agent's replaces it, so one process's settings never answer for another's; a hold
/// says whether the default moved. The window asks for
/// a read once while one is due, not on every frame. The table is bounded. NEGATIVE CONTROL:
/// another tab, and the same tab's other agent, read nothing.
#[test]
fn a_default_is_held_for_its_agent_and_asked_for_once_while_due() {
    let mut reads = DefaultReads::default();
    let t0 = Instant::now();
    assert!(reads.due("s-a", 10, t0), "never read");
    assert_eq!(reads.get("s-a", 10), None);
    assert!(reads.ask("s-a", 10, t0), "the first ask");
    assert!(
        !reads.ask("s-a", 10, t0 + DEFAULT_RECHECK / 2),
        "asked lately"
    );
    assert!(
        reads.hold("s-a", 10, t0, default_of(Resolved::Unset)),
        "a first read moves it"
    );
    assert!(!reads.due("s-a", 10, t0 + DEFAULT_RECHECK / 2));
    assert!(
        !reads.ask("s-a", 10, t0 + DEFAULT_RECHECK / 2),
        "nothing due"
    );
    assert_eq!(reads.get("s-a", 10), Some(default_of(Resolved::Unset)));
    assert!(
        reads.due("s-a", 10, t0 + DEFAULT_RECHECK),
        "stood the recheck"
    );
    assert!(
        reads.ask("s-a", 10, t0 + DEFAULT_RECHECK),
        "asked again once due"
    );
    assert!(
        !reads.hold("s-a", 10, t0 + DEFAULT_RECHECK, default_of(Resolved::Unset)),
        "the same default read again moves nothing"
    );
    let plan = default_of(Resolved::Mode {
        mode: aterm_agent::harness::lights::Mode::Plan,
        source: "~/.claude/settings.json".to_owned(),
    });
    assert!(reads.hold("s-a", 10, t0 + DEFAULT_RECHECK * 2, plan.clone()));
    assert_eq!(reads.get("s-a", 10), Some(plan.clone()));
    assert_eq!(reads.get("s-b", 10), None, "NEGATIVE CONTROL: another tab");
    assert_eq!(
        reads.get("s-a", 11),
        None,
        "NEGATIVE CONTROL: another agent"
    );
    assert!(
        reads.due("s-a", 11, t0),
        "a relaunched agent is not read yet"
    );
    assert!(reads.hold("s-a", 11, t0, default_of(Resolved::Unset)));
    assert_eq!(reads.get("s-a", 10), None, "the ended agent's read is gone");
    assert_eq!(reads.0.len(), 1, "one agent a tab");
    for n in 0..DEFAULT_READS_MAX * 2 {
        reads.hold(&format!("s-{n}"), 1, t0, default_of(Resolved::Unset));
    }
    assert_eq!(reads.0.len(), DEFAULT_READS_MAX, "bounded");
    let last = format!("s-{}", DEFAULT_READS_MAX * 2 - 1);
    assert!(reads.get(&last, 1).is_some(), "the newest is kept");
}

/// THE WINDOW THREAD READS NO SETTINGS FILE FOR A CLICK: the production default of a tab whose
/// agent a relaunch snapshot names is what the resolver last held for that agent, and before
/// its first read the window holds none, rather than reading the
/// agent's settings chain itself (the defect: a click read the agent's `.claude` files, its git
/// worktree and the user and managed files on the window thread, which a dead network mount
/// under the agent's directory froze). Once the resolver holds a read, the window answers it.
#[test]
fn the_window_answers_the_default_from_the_resolvers_read() {
    let tab = "s-window-reads-no-file";
    let pid = 4_000_123;
    let mut snap = aterm_agent::harness::relaunch::Snapshot {
        launch_env: None,
        auth_observed: true,
        route: aterm_agent::harness::accounts::AuthRoute::Outside,
        profile: None,
        tab: tab.to_owned(),
        pid,
        lifetime: None,
        start: "Thu Sep 24 01:02:03 2026".to_owned(),
        shell: 4242,
        program: std::path::PathBuf::from("/opt/claude"),
        argv: vec!["/opt/claude".to_owned()],
        session: None,
        cwd: "/".to_owned(),
        version: Some("2.1.318".to_owned()),
        run: aterm_agent::harness::relaunch::AgentRun::Claude,
        dialect: None,
        login: None,
        home: None,
        claude_dir: None,
    };
    crate::harness_host::publish_snapshot_for_test(tab, Some(&snap));
    let defaults = live_defaults();
    assert_eq!(
        (defaults.0)(tab),
        None,
        "before the resolver's read the window reads nothing"
    );
    let plan = default_of(Resolved::Mode {
        mode: aterm_agent::harness::lights::Mode::Plan,
        source: "~/.claude/settings.json".to_owned(),
    });
    DEFAULT_READS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .hold(tab, pid, Instant::now(), plan.clone());
    assert_eq!((defaults.0)(tab), Some(plan), "the resolver's read answers");
    snap.pid = pid + 1;
    crate::harness_host::publish_snapshot_for_test(tab, Some(&snap));
    assert_eq!(
        (defaults.0)(tab),
        None,
        "NEGATIVE CONTROL: a relaunched agent's default is not the ended one's"
    );
    crate::harness_host::publish_snapshot_for_test(tab, None);
}
