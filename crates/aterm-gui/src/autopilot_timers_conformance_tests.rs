// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TIER-1 FOR `AutopilotTimer` (`aterm_models_gui::autopilot_timer_model`;
//! `docs/DESIGN-harness-autopilot-2026-10-15.md` §9.3.8): the REAL index
//! (`AutopilotTimers`), the window's writer, the wire (`meta set|unset
//! autopilot … [until=]`), the App's wake (`on_meta_changed`, dropped before a
//! successor's Commit by the real `admit_wake_before_commit`), the fire
//! (`expire_autopilot_timers` → `session_timeline::fire_land_at`), the moved
//! wall, the restore leaf's graft, the Commit's rebuild and a session's
//! close, driven step by step on pinned clocks — every observed transition
//! validated against the model's action of the same name (the interpreter,
//! and `ty` where it is installed), and every invariant checked after it.
//!
//! NEGATIVE CONTROLS, each of which the same model must refuse: a fire that
//! trusts its index (it loses to an in-flight Off), a same-word write that
//! keeps the timer, a fire on the monotonic deadline alone after the clock
//! was set back, and a Commit that skips the rebuild.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use aterm_agent::supervise::Autopilot;
use aterm_spec::derive::Model;

use super::set_fake_clocks;
use crate::App;
use crate::session_timeline::MetaField;

/// One model minute of the wall and the monotonic clock, in ms.
const MINUTE: i64 = 60_000;

/// The wall clock's origin, unix ms (2026-10-16T19:00:00Z): a whole second.
const WALL0: i64 = 1_792_177_200_000;

/// The REAL parts of the timer, driven step by step, with the model's view
/// of what the code does not keep (the clocks it reads, the held wake, the
/// leaf a successor owes, the ghosts).
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
    /// The leaf a successor owes the session: `(word, land_at)` as the model
    /// counts them.
    leaf: Option<(i64, i64)>,
    hand: i64,
    fires: i64,
    over_hand: i64,
    early: i64,
}

/// A test App with a host, so the harness carries a mode.
fn hosted() -> (App, crate::harness_host::HostHandle) {
    let mut app = App::headless_for_test();
    let host = crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    );
    app.harness = Some(host.clone());
    (app, host)
}

/// The model's number for a stored word.
fn word_number(word: Option<Autopilot>) -> i64 {
    match word {
        None => 0,
        Some(Autopilot::Cruise) => 1,
        Some(Autopilot::Land) => 2,
        Some(Autopilot::Off) => 3,
    }
}

/// The model's minute for a stored instant.
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

    /// The real state, projected onto the model's variables.
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

    /// Run `act` on the real code and validate the observed transition
    /// against `action` (both tiers), then every invariant; a step that moves
    /// nothing is a stutter. The model must enable `action` first.
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

    /// The wire's `meta` on the session, as the dispatch arm runs it; its
    /// `MetaChanged` is held (`wake`) until [`Self::apply_wake`].
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
            d.app
                .set_session_autopilot_timed(d.session, Some(Autopilot::Cruise), Some(at));
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

    /// The real fire, observed: a Land it wrote is counted against the arm,
    /// and against a stored hand write or a wall short of the instant.
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

    /// The restore leaf the capture writes for the session now
    /// (`SessionMeta::sanitized`, the capture's own), in the model's numbers.
    fn capture(&self) -> crate::restore::TerminalLeafRestore {
        let meta = self.ctx().unwrap().meta.lock().unwrap().sanitized();
        crate::restore::TerminalLeafRestore {
            sid: None,
            lineage: None,
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
            identity: None,
            agent: None,
            supervised: None,
            held: false,
            holds: Vec::new(),
            theme: None,
        }
    }

    /// A new process stands in for this one (a fresh App and host, the
    /// session its window's bootstrap), the old one shut down.
    fn new_process(&mut self, successor: bool) {
        let (mut app, host) = hosted();
        if successor {
            app.handoff_successor = true;
            app.incoming_handoff_pending = true;
        }
        let old_host = std::mem::replace(&mut self.host, host);
        old_host.shutdown_and_join();
        // The session the leaf names: a pane of its own, closable alone.
        self.session = app.split_active_stub_tab(crate::WindowId(0));
        self.app = app;
        self.wake = false;
    }

    /// aterm quits and comes back: the leaf captured at the quit is seeded
    /// onto the new process's session and indexed (`graft_restored_user_meta`,
    /// the path window 0's bootstrap takes).
    fn close_and_restore(&mut self) {
        self.step("CloseAndRestore", |d| {
            let leaf = d.capture();
            d.new_process(false);
            d.app.graft_restored_user_meta(d.session, &leaf);
        });
    }

    /// A seamless update: the successor adopts the session with a fresh store
    /// and owes it the leaf.
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

    /// The live graft of the leaf the successor owes.
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

    /// The Commit: `Wake::ActivateCommittedHandoff` lifts the gate, and the
    /// park after it settles the index the Commit owes
    /// (`App::settle_autopilot_commit`, the first step of every
    /// `expire_autopilot_timers`) — driven here apart from that park's fire,
    /// which the model counts as its own `Fire`.
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

    /// `mutant` applied to a copy of the real state must be refused as
    /// `action` by the model.
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

/// THE BIND: every lane of §9.3.6 driven through the real code and held to
/// the model — the window's arm and its fire; the wire's arm, its wake and the
/// fire; a stored Off racing an armed entry; the same word cancelling; the
/// clock set back at the fire; asleep through the time and the wall's
/// re-derivation; a cold restore across the instant; a seamless update whose
/// wire write lands before the Commit, a driver's write winning over the
/// graft, and the Commit's rebuild; and a close with a timer armed. Then the
/// four negative controls, each refused by the same model.
#[test]
fn the_real_timer_conforms_to_its_model() {
    let mut d = TimerDrive::new();
    assert_eq!(d.state(), d.model.init_state(), "the model's start");

    // The window arms; the time comes; Land.
    d.set_cruise_until();
    d.tick();
    d.tick();
    d.fire();
    assert_eq!(d.state()["word"], 2);
    d.clock_back();
    d.clock_back();

    // The wire arms; its wake; the fire.
    d.wire_store_until();
    d.apply_wake();
    d.tick();
    d.tick();
    d.fire();
    assert_eq!(d.state()["word"], 2);
    d.clock_back();
    d.clock_back();

    // The race: armed; an Off stored over the wire, its wake held; the fire
    // reads the store and writes nothing; the wake then lands.
    d.set_cruise_until();
    d.wire_hand("WireOff", "set autopilot off");
    d.tick();
    d.tick();
    d.fire();
    assert_eq!(d.state()["word"], 3, "the hand wins");
    d.apply_wake();
    d.clock_back();
    d.clock_back();

    // The same word cancels.
    d.set_cruise_until();
    d.wire_hand("WireCruise", "set autopilot cruise");
    d.apply_wake();
    assert_eq!((d.state()["land_at"], d.state()["idx"]), (0, 0));

    // The clock set back under a due entry: no Land; the remainder indexed.
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

    // Asleep through the time: the wall re-read, then the fire.
    d.set_cruise_until();
    d.sleep();
    d.sleep();
    d.wall_moved();
    d.fire();
    assert_eq!(d.state()["word"], 2);
    d.clock_back();
    d.clock_back();

    // A cold restore across the instant: carried, indexed due at once,
    // landed.
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

    // A seamless update: armed; the successor grafts and indexes; the time
    // comes before its Commit and nothing fires; the Commit lands it.
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

    // Another update: a driver's Off on the successor before the graft wins
    // for both halves; an arm over the wire before the Commit is indexed by
    // its rebuild.
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

    // Closed with a timer armed: its entry goes; nothing is due after.
    d.set_cruise_until();
    d.close_session();
    assert_eq!(d.app.autopilot_timer_deadline(), None);
    d.tick();
    d.tick();

    // NEGATIVE CONTROLS on a fresh drive.
    let mut d = TimerDrive::new();
    d.set_cruise_until();
    d.wire_hand("WireOff", "set autopilot off");
    d.tick();
    d.tick();
    // 1. A fire that trusts its index: Land written over the stored Off.
    let before = d.state();
    let ctx = d.ctx().unwrap();
    crate::session_timeline::apply_autopilot(&ctx, Some("land".into()), None);
    d.app.autopilot_timers.forget(d.session);
    let mut after = d.state();
    after.insert("fires", 1);
    after.insert("over_hand", 1);
    d.refuses(&before, &after, "Fire", "a fire that trusts its index");
    // 2. A same-word write that keeps the timer.
    let mut d = TimerDrive::new();
    d.set_cruise_until();
    let before = d.state();
    {
        let ctx = d.ctx().unwrap();
        let mut meta = ctx.meta.lock().unwrap();
        // The defect: the word compared alone, the timer kept.
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
    // 3. A fire on the monotonic deadline alone, after the clock went back.
    d.tick();
    d.tick();
    d.clock_back();
    let before = d.state();
    let ctx = d.ctx().unwrap();
    let entry = *d.app.autopilot_timers.entry(d.session).unwrap();
    assert_eq!(
        crate::session_timeline::fire_land_at(&ctx, entry.land_at_ms, i64::MAX),
        crate::session_timeline::Fired::Landed,
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
    // 4. A Commit that skips the rebuild.
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
