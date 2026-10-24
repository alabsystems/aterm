// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance tests for `AutopilotTimer` against `aterm_models_gui::autopilot_timer_model`.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use aterm_agent::supervise::Autopilot;
use aterm_spec::derive::Model;

use super::set_fake_clocks;
use crate::App;
use crate::session_timeline::MetaField;

const MINUTE: i64 = 60_000;

const WALL0: i64 = 1_792_177_200_000;

struct TimerDrive {
    app: App,
    host: crate::harness_host::HostHandle,
    /// The session the drive writes and reads.
    session: u64,
    model: Model,
    mono0: Instant,
    /// Monotonic minutes since `mono0`.
    mono: i64,
    /// Wall minutes since `WALL0`.
    wall: i64,
    /// A wire write's `MetaChanged`, posted and not yet handled.
    wake: bool,
    /// The leaf a successor owes the session: `(word, land_at)` as the model counts them.
    leaf: Option<(i64, i64)>,
    hand: i64,
    fires: i64,
    over_hand: i64,
    early: i64,
}

fn hosted() -> (App, crate::harness_host::HostHandle) {
    let mut app = App::headless_for_test();
    let host = crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    );
    app.harness = Some(host.clone());
    (app, host)
}

fn word_number(word: Option<Autopilot>) -> i64 {
    match word {
        None => 0,
        Some(Autopilot::Cruise) => 1,
        Some(Autopilot::Land) => 2,
        Some(Autopilot::Off) => 3,
    }
}

fn minute_of(ms: Option<i64>) -> i64 {
    ms.map_or(0, |ms| (ms - WALL0) / MINUTE)
}

impl TimerDrive {
    fn new() -> Self {
        let (mut app, host) = hosted();
        let session = app.split_active_stub_tab(crate::WindowId(0));
        let drive = Self {
            app,
            host,
            session,
            model: aterm_models_gui::autopilot_timer_model(),
            mono0: Instant::now(),
            mono: 0,
            wall: 0,
            wake: false,
            leaf: None,
            hand: 0,
            fires: 0,
            over_hand: 0,
            early: 0,
        };
        drive.pin_clocks();
        drive
    }

    fn pin_clocks(&self) {
        set_fake_clocks(Some((
            self.mono0 + Duration::from_millis(u64::try_from(self.mono * MINUTE).unwrap()),
            WALL0 + self.wall * MINUTE,
        )));
    }

    fn now(&self) -> Instant {
        self.mono0 + Duration::from_millis(u64::try_from(self.mono * MINUTE).unwrap())
    }

    fn ctx(&self) -> Option<std::sync::Arc<crate::SessionCtx>> {
        self.app.pool.get(self.session).map(|s| s.ctx.clone())
    }

    fn state(&self) -> BTreeMap<&'static str, i64> {
        let (word, land_at, bit) = self.ctx().map_or((0, 0, 0), |ctx| {
            let meta = ctx.meta.lock().unwrap();
            (
                word_number(meta.autopilot_word()),
                minute_of(meta.land_at),
                i64::from(meta.driver_wrote(MetaField::Autopilot)),
            )
        });
        let (idx, wait) = self
            .app
            .autopilot_timers
            .entry(self.session)
            .map_or((0, 0), |entry| {
                let ahead = entry.due.saturating_duration_since(self.now());
                let ahead = i64::try_from(ahead.as_millis()).unwrap();
                (
                    minute_of(Some(entry.land_at_ms)),
                    (ahead + MINUTE - 1) / MINUTE,
                )
            });
        let (leaf, leaf_word, leaf_land) = self.leaf.map_or((0, 0, 0), |(w, l)| (1, w, l));
        BTreeMap::from([
            ("word", word),
            ("land_at", land_at),
            ("idx", idx),
            ("wait", wait),
            ("wake", i64::from(self.wake)),
            ("wall", self.wall),
            ("committed", i64::from(!self.app.incoming_handoff_pending)),
            (
                "alive",
                i64::from(self.app.pool.get(self.session).is_some()),
            ),
            ("bit", bit),
            ("leaf", leaf),
            ("leaf_word", leaf_word),
            ("leaf_land", leaf_land),
            ("hand", self.hand),
            ("fires", self.fires),
            ("over_hand", self.over_hand),
            ("early", self.early),
        ])
    }

    fn step(&mut self, action: &str, act: impl FnOnce(&mut Self)) {
        let before = self.state();
        assert!(
            self.model.action_enabled(action, &before),
            "{action} is not enabled in {before:?}"
        );
        act(self);
        self.pin_clocks();
        let after = self.state();
        if before == after {
            return;
        }
        let (conforms, evidence) = aterm_spec::verify::validate_transition_tiered(
            &self.model,
            &[],
            &before,
            &after,
            Some(action),
            action,
        );
        assert!(conforms, "{action}: {before:?} -> {after:?}: {evidence}");
        for invariant in &self.model.invariants {
            assert!(
                self.model.check_invariant(invariant.name, &after),
                "{action}: {} fails in {after:?}",
                invariant.name
            );
        }
    }

    fn wire(&mut self, rest: &str) -> String {
        let session = self.app.pool.get(self.session).unwrap();
        let (term, ctx) = (session.term.clone(), session.ctx.clone());
        let (reply, changed) =
            crate::control::cmd_meta(&term, &self.app.store, self.session, &ctx, rest);
        assert!(reply.starts_with("OK"), "{rest}: {reply}");
        self.wake |= changed;
        reply
    }

    fn set_cruise_until(&mut self) {
        self.step("SetCruiseUntil", |d| {
            let at = WALL0 + (d.wall + 2) * MINUTE;
            d.app.set_session_autopilot_timed(
                d.session,
                Some(Autopilot::Cruise),
                Some(at),
                crate::autopilot::Surface::Menu,
            );
            d.hand = 0;
            d.fires = 0;
        });
    }

    fn wire_store_until(&mut self) {
        self.step("WireStoreUntil", |d| {
            let reply = d.wire("set autopilot cruise until=+2m");
            assert!(reply.starts_with("OK land_at="), "{reply}");
            d.hand = 0;
            d.fires = 0;
        });
    }

    fn wire_hand(&mut self, action: &str, rest: &str) {
        self.step(action, |d| {
            d.wire(rest);
            d.hand = 1;
        });
    }

    fn apply_wake(&mut self) {
        self.step("ApplyWake", |d| {
            d.wake = false;
            let wake = crate::Wake::MetaChanged { session: d.session };
            if d.app.admit_wake_before_commit(wake).is_some() {
                d.app.on_meta_changed(d.session);
            }
        });
    }

    fn tick(&mut self) {
        self.step("Tick", |d| {
            d.wall += 1;
            d.mono += 1;
        });
    }

    fn sleep(&mut self) {
        self.step("Sleep", |d| d.wall += 1);
    }

    fn clock_back(&mut self) {
        self.step("ClockBack", |d| d.wall -= 1);
    }

    fn fire(&mut self) {
        let before = self.state();
        self.step("Fire", |d| {
            d.pin_clocks();
            d.app.expire_autopilot_timers();
            let landed = d.ctx().is_some_and(|ctx| {
                ctx.meta.lock().unwrap().autopilot_word() == Some(Autopilot::Land)
            });
            if landed && before["word"] == 1 {
                d.fires = (d.fires + 1).min(2);
                if d.hand == 1 {
                    d.over_hand = 1;
                }
                if before["land_at"] > d.wall {
                    d.early = 1;
                }
            }
        });
    }

    fn wall_moved(&mut self) {
        self.step("WallMoved", |d| d.app.on_wall_moved());
    }

    fn capture(&self) -> crate::restore::TerminalLeafRestore {
        let meta = self.ctx().unwrap().meta.lock().unwrap().sanitized();
        crate::restore::TerminalLeafRestore {
            sid: None,
            lineage: None,
            log_ep: None,
            cwd: None,
            title: String::new(),
            profile: None,
            local_id: Some(0),
            user_title: None,
            description: None,
            icon: None,
            role: None,
            attention: None,
            questions: None,
            autopilot: meta.autopilot,
            land_at: meta.land_at,
            autopilot_by: None,
            agent: None,
            supervised: None,
            lost: None,
            held: false,
            holds: Vec::new(),
            theme: None,
        }
    }

    fn new_process(&mut self, successor: bool) {
        let (mut app, host) = hosted();
        if successor {
            app.handoff_successor = true;
            app.incoming_handoff_pending = true;
        }
        let old_host = std::mem::replace(&mut self.host, host);
        old_host.shutdown_and_join();
        self.session = app.split_active_stub_tab(crate::WindowId(0));
        self.app = app;
        self.wake = false;
    }

    fn close_and_restore(&mut self) {
        self.step("CloseAndRestore", |d| {
            let leaf = d.capture();
            d.new_process(false);
            d.app.graft_restored_user_meta(d.session, &leaf);
        });
    }

    fn handoff_start(&mut self) {
        self.step("HandoffStart", |d| {
            let leaf = d.capture();
            d.leaf = Some((
                word_number(leaf.autopilot.as_deref().and_then(Autopilot::parse)),
                minute_of(leaf.land_at),
            ));
            d.new_process(true);
        });
    }

    fn graft(&mut self) {
        self.step("Graft", |d| d.graft_now());
    }

    fn graft_now(&mut self) {
        let (word, land) = self.leaf.expect("a leaf is owed");
        let mut leaf = self.capture();
        leaf.autopilot = match word {
            1 => Some("cruise".into()),
            2 => Some("land".into()),
            3 => Some("off".into()),
            _ => None,
        };
        leaf.land_at = (land > 0).then_some(WALL0 + land * MINUTE);
        self.app.graft_restored_user_meta(self.session, &leaf);
    }

    fn commit(&mut self) {
        self.step("HandoffCommit", |d| {
            d.app.incoming_handoff_pending = false;
            d.app.settle_autopilot_commit();
            d.leaf = None;
            d.wake = false;
        });
    }

    fn close_session(&mut self) {
        self.step("CloseSession", |d| {
            let closed = d.app.close_session_by_id(d.session);
            assert!(closed.is_ok(), "{closed:?}");
            assert!(
                d.app.pool.get(d.session).is_none(),
                "closed now, not deferred"
            );
            d.wake = false;
        });
    }

    fn refuses(
        &self,
        before: &BTreeMap<&'static str, i64>,
        after: &BTreeMap<&'static str, i64>,
        action: &str,
        why: &str,
    ) {
        let (conforms, evidence) = aterm_spec::verify::validate_transition_tiered(
            &self.model,
            &[],
            before,
            after,
            Some(action),
            why,
        );
        assert!(!conforms, "{why} conformed as {action}: {evidence}");
    }
}

#[test]
fn the_real_timer_conforms_to_its_model() {
    let mut d = TimerDrive::new();
    assert_eq!(d.state(), d.model.init_state(), "the model's start");

    d.set_cruise_until();
    d.tick();
    d.tick();
    d.fire();
    assert_eq!(d.state()["word"], 2);
    d.clock_back();
    d.clock_back();

    d.wire_store_until();
    d.apply_wake();
    d.tick();
    d.tick();
    d.fire();
    assert_eq!(d.state()["word"], 2);
    d.clock_back();
    d.clock_back();

    d.set_cruise_until();
    d.wire_hand("WireOff", "set autopilot off");
    d.tick();
    d.tick();
    d.fire();
    assert_eq!(d.state()["word"], 3, "the hand wins");
    d.apply_wake();
    d.clock_back();
    d.clock_back();

    d.set_cruise_until();
    d.wire_hand("WireCruise", "set autopilot cruise");
    d.apply_wake();
    assert_eq!((d.state()["land_at"], d.state()["idx"]), (0, 0));

    d.set_cruise_until();
    d.tick();
    d.tick();
    d.clock_back();
    d.fire();
    assert_eq!((d.state()["word"], d.state()["wait"]), (1, 1));
    d.tick();
    d.fire();
    assert_eq!(d.state()["word"], 2);
    d.clock_back();
    d.clock_back();

    d.set_cruise_until();
    d.sleep();
    d.sleep();
    d.wall_moved();
    d.fire();
    assert_eq!(d.state()["word"], 2);
    d.clock_back();
    d.clock_back();

    d.wire_store_until();
    d.apply_wake();
    d.close_and_restore();
    d.sleep();
    d.sleep();
    d.wall_moved();
    d.fire();
    assert_eq!(d.state()["word"], 2);
    d.clock_back();
    d.clock_back();

    d.set_cruise_until();
    d.handoff_start();
    d.graft();
    assert_eq!(d.state()["idx"], 2, "the graft indexes the carried timer");
    d.tick();
    d.tick();
    assert!(
        !d.model.action_enabled("Fire", &d.state()),
        "no fire before Commit"
    );
    d.pin_clocks();
    d.app.expire_autopilot_timers();
    assert_eq!(d.state()["word"], 1, "the real code fires nothing either");
    d.commit();
    d.fire();
    assert_eq!(d.state()["word"], 2);
    d.clock_back();
    d.clock_back();

    d.set_cruise_until();
    d.handoff_start();
    d.wire_hand("WireOff", "set autopilot off");
    let before = d.state();
    d.graft_now();
    assert_eq!(
        d.state(),
        before,
        "the driver's write stands: the graft is no step"
    );
    d.wire_store_until();
    d.apply_wake();
    assert_eq!(
        d.state()["idx"],
        0,
        "the wake was dropped before the Commit"
    );
    d.commit();
    assert_eq!(d.state()["idx"], 2, "rebuilt at the Commit");
    d.tick();
    d.tick();
    d.fire();
    assert_eq!(d.state()["word"], 2);
    d.clock_back();
    d.clock_back();

    d.set_cruise_until();
    d.close_session();
    assert_eq!(d.app.autopilot_timer_deadline(), None);
    d.tick();
    d.tick();

    let mut d = TimerDrive::new();
    d.set_cruise_until();
    d.wire_hand("WireOff", "set autopilot off");
    d.tick();
    d.tick();
    let before = d.state();
    let ctx = d.ctx().unwrap();
    crate::session_timeline::apply_autopilot(
        &ctx,
        Some("land".into()),
        None,
        aterm_slog::Actor::Ctl,
        None,
    );
    d.app.autopilot_timers.forget(d.session);
    let mut after = d.state();
    after.insert("fires", 1);
    after.insert("over_hand", 1);
    d.refuses(&before, &after, "Fire", "a fire that trusts its index");
    let mut d = TimerDrive::new();
    d.set_cruise_until();
    let before = d.state();
    {
        let ctx = d.ctx().unwrap();
        let mut meta = ctx.meta.lock().unwrap();
        meta.autopilot = Some("cruise".into());
    }
    let mut after = d.state();
    after.insert("hand", 1);
    d.refuses(
        &before,
        &after,
        "WireCruise",
        "a same-word write that keeps the timer",
    );
    d.tick();
    d.tick();
    d.clock_back();
    let before = d.state();
    let ctx = d.ctx().unwrap();
    let entry = *d.app.autopilot_timers.entry(d.session).unwrap();
    assert!(
        matches!(
            crate::session_timeline::fire_land_at(&ctx, entry.land_at_ms, i64::MAX),
            crate::session_timeline::Fired::Landed { .. }
        ),
        "the defect: the wall unread"
    );
    d.app.autopilot_timers.forget(d.session);
    let mut after = d.state();
    after.insert("fires", 1);
    after.insert("early", 1);
    d.refuses(
        &before,
        &after,
        "Fire",
        "a fire on the monotonic deadline alone",
    );
    let mut d = TimerDrive::new();
    d.set_cruise_until();
    d.handoff_start();
    d.wire_store_until();
    d.apply_wake();
    let before = d.state();
    d.app.incoming_handoff_pending = false;
    d.leaf = None;
    d.wake = false;
    let after = d.state();
    d.refuses(
        &before,
        &after,
        "HandoffCommit",
        "a Commit that skips the rebuild",
    );
    set_fake_clocks(None);
    d.host.shutdown_and_join();
}
