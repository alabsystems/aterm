// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 bind for `ChromeRegrowFloor` (`aterm_spec::derive::chrome_regrow_floor_model`):
//! after any rise of a window's chrome rows, no chrome source's quiet fall re-grids the
//! window back inside `CHROME_REGROW_SETTLE`.
//!
//! The rig drives the shipping band (`MessageCenter::post`/`dismiss`,
//! `App::settle_messages`) and presence row (a turn's lease, `App::on_presence_wake`,
//! `App::tell_story`, the test-only `App::note_human_acted`) on one clock, against a
//! headless window standing in for glass (`chrome_regrids_for_test`). After each step the
//! host is projected onto the model ([`project`]): an event or a passage of time must be
//! its model action, a wake that re-grids must be a fall the model admits, and a wake that
//! declines must decline exactly where the model enables no fall. Negative control: on a
//! window with no floor each cross-source schedule re-grids back inside the settle, which
//! the committed model rejects and `Buggy = 1` admits only as `BandFallOwnQuiet` or
//! `PresFallOwnQuiet`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aterm_messages::chrome::CHROME_REGROW_SETTLE;
use aterm_messages::presence::drive::{self, Desk};
use aterm_messages::{Message, MessageId, SHRINK_QUIET, Severity, WallStamp, tags};
use aterm_spec::derive::{Model, chrome_regrow_floor_model};
use aterm_spec::interp::{self, State};

use crate::presence::StoryVerb;
use crate::{App, Lease, SessionCtx, WindowId};

const STAMP: WallStamp = WallStamp {
    unix_ms: 1_790_000_000_000,
};

/// What the rig saw: the projection's ground truth.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Seen {
    /// When the window's chrome total last rose.
    rise: Option<Instant>,
    /// An unforced fall landed inside the settle of that rise.
    quick: bool,
    /// The rig took the window's room away ([`Step::Cramp`]).
    cramped: bool,
    /// The environment's moves so far.
    flips: i64,
}

/// `ChromeRegrowFloor`'s state for window `wid` at `now`.
pub(crate) fn project(app: &App, wid: WindowId, now: Instant, seen: &Seen) -> State {
    let ws = app.windows.get(&wid).expect("the rig's window");
    let v = &ws.presence;
    let bw = app.messages.wanted_rows() > 0;
    let b = app.message_band_rows > 0;
    let pw = v.words.is_some();
    let p = v.rows > 0;
    // A source's own quiet reads as run unless a fall of it is pending and its own due is
    // still ahead.
    let bq = !(b && !bw) || app.messages.quiet_shrink_at().is_some_and(|x| now >= x);
    let pq = !(p && !pw) || drive::fold_due(v, app.looked_at(wid)).is_some_and(|x| now >= x);
    let young = |at: Option<Instant>| at.is_some_and(|x| now < x);
    let mut s = State::new();
    s.insert("bw", i64::from(bw));
    s.insert("b", i64::from(b));
    s.insert("pw", i64::from(pw));
    s.insert("p", i64::from(p));
    s.insert("bq", i64::from(bq));
    s.insert("pq", i64::from(pq));
    s.insert(
        "age",
        i64::from(!young(seen.rise.map(|r| r + CHROME_REGROW_SETTLE))),
    );
    s.insert("since", i64::from(!young(ws.regrow_floor.floor())));
    s.insert("cramped", i64::from(seen.cramped));
    s.insert("quick", i64::from(seen.quick));
    s.insert("flips", seen.flips);
    s
}

/// Whether `action` takes `before` to `after` in `model`.
fn fires(model: &Model, action: &str, before: &State, after: &State) -> bool {
    model.successors(action, before).contains(after)
}

/// Whether `first` then `then` take `before` to `after` in `model`.
fn fires_both(model: &Model, first: &str, then: &str, before: &State, after: &State) -> bool {
    model
        .successors(first, before)
        .iter()
        .any(|mid| fires(model, then, mid, after))
}

/// One step of a schedule.
#[derive(Clone, Copy, Debug)]
enum Step {
    /// A band row reaches the glass (posted and settled).
    BandRise,
    /// The band row is dismissed (its want drops; its quiet starts).
    BandUnwant,
    /// A turn's lease raises the presence row.
    PresRiseLease,
    /// A told story raises the presence row.
    PresRiseStory,
    /// The turn's lease ends: the presence row's want drops.
    PresUnwant,
    /// A person's keystroke reads the story (the fold law).
    PresRead,
    /// The window's frame loses its room (a drag or zoom `on_resize` books): the stand-in's
    /// terminal rows drop to none, so its grid can afford neither chrome row.
    Cramp,
    /// Time passes to the given offset past the rig's origin, running no
    /// host code: the model's action it is, or `None` for a span in which
    /// nothing the model reads moves (a stutter).
    At(Duration, Option<&'static str>),
    /// Time passes to the given offset past the latest rise the rig saw
    /// (the floor's tick), running no host code.
    AfterRise(Duration, &'static str),
    /// The loop's wake at the rig's instant.
    Wake,
}

struct Rig {
    app: App,
    wid: WindowId,
    ctx: Arc<SessionCtx>,
    session: u64,
    origin: Instant,
    now: Instant,
    band: Option<MessageId>,
    seen: Seen,
    total: u16,
    model: Model,
    buggy: Model,
    /// Every step's `(step, before, after)`.
    trace: Vec<(Step, State, State)>,
}

impl Rig {
    fn new(floored: bool) -> Self {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let session = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(session));
        let ctx = app.pool.get(session).expect("stub pooled").ctx.clone();
        app.windows
            .get_mut(&wid)
            .expect("window 0")
            .chrome_regrids_for_test = floored;
        let origin = Instant::now();
        let model = chrome_regrow_floor_model();
        let buggy = interp::with_buggy(&model, 1);
        let total = app.chrome_rows(wid);
        let mut rig = Rig {
            app,
            wid,
            ctx,
            session,
            origin,
            now: origin,
            band: None,
            seen: Seen::default(),
            total,
            model,
            buggy,
            trace: Vec::new(),
        };
        rig.set_clock(origin);
        assert_eq!(
            rig.state(),
            rig.model.init_state(),
            "the rig starts where the model does"
        );
        rig
    }

    fn set_clock(&mut self, now: Instant) {
        self.now = now;
        crate::app_presence::set_presence_clock_for_test(Some(now));
    }

    fn state(&self) -> State {
        project(&self.app, self.wid, self.now, &self.seen)
    }

    /// The window's floor as the engine counts it.
    fn floor(&self) -> Option<Instant> {
        self.app.windows[&self.wid].regrow_floor.floor()
    }

    /// Book what the step did to the chrome total: a rise anchors the ground truth, and an
    /// unforced fall inside its settle is `quick` (a fall on a cramped window is the
    /// window's own, exempt).
    fn book_total(&mut self) {
        let total = self.app.chrome_rows(self.wid);
        if total > self.total {
            self.seen.rise = Some(self.now);
        } else if total < self.total
            && !self.seen.cramped
            && self
                .seen
                .rise
                .is_some_and(|r| self.now < r + CHROME_REGROW_SETTLE)
        {
            self.seen.quick = true;
        }
        self.total = total;
    }

    /// The loop's wake at the rig's instant: the timer's sweep (band step, then presence
    /// tick), the focus/restore funnel that projects every window whatever its deadline (so
    /// a fall the floor holds is judged by the commit itself), and the park's convergence
    /// of the band's count.
    fn wake(&mut self) {
        let now = self.now;
        let _ = self.app.settle_messages(now);
        let _ = self.app.presence_tick(now);
        self.app.refresh_presence_all_windows();
        let _ = self.app.sync_message_band_rows(now);
    }

    fn step(&mut self, step: Step) {
        let before = self.state();
        match step {
            Step::BandRise => {
                let msg = Message::new(tags::CONFIG, Severity::Warn, "Disk nearly full").now(
                    aterm_messages::need::Ask::flash(
                        aterm_messages::need::Why::OnlyYou(aterm_messages::need::Reason::new(
                            "the disk is nearly full",
                        )),
                        aterm_messages::need::Then::step("Free some space"),
                        aterm_messages::need::Span::Ten,
                    ),
                );
                let posted = self.app.messages.post(msg, STAMP, self.now);
                self.band = Some(posted.id);
                let _ = self.app.settle_messages(self.now);
                self.seen.flips += 1;
            }
            Step::BandUnwant => {
                let id = self.band.take().expect("a band row to dismiss");
                assert!(self.app.messages.dismiss(id, self.now));
                let _ = self.app.settle_messages(self.now);
                self.seen.flips += 1;
            }
            Step::PresRiseLease => {
                *self.ctx.turn_lease.lock().unwrap() = Some(Lease::Turn {
                    id: 7,
                    driver: None,
                    typing: false,
                });
                self.app.on_presence_wake(&self.ctx.self_id, false);
                self.seen.flips += 1;
            }
            Step::PresRiseStory => {
                self.app
                    .tell_story(self.session, StoryVerb::Approval, "")
                    .expect("the stub is held");
                self.seen.flips += 1;
            }
            Step::PresUnwant => {
                *self.ctx.turn_lease.lock().unwrap() = None;
                self.app.on_presence_wake(&self.ctx.self_id, false);
                self.seen.flips += 1;
            }
            Step::PresRead => {
                self.app.note_human_acted(self.wid);
                self.seen.flips += 1;
            }
            Step::Cramp => {
                self.app
                    .windows
                    .get_mut(&self.wid)
                    .expect("the rig's window")
                    .rows = 0;
                self.seen.cramped = true;
                self.seen.flips += 1;
            }
            Step::At(offset, _) => self.set_clock(self.origin + offset),
            Step::AfterRise(offset, _) => {
                let rise = self.seen.rise.expect("a rise to count from");
                self.set_clock(rise + offset);
            }
            Step::Wake => self.wake(),
        }
        self.book_total();
        let after = self.state();
        self.trace.push((step, before, after));
    }

    fn run(&mut self, schedule: &[Step]) {
        for step in schedule {
            self.step(*step);
        }
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        crate::app_presence::set_presence_clock_for_test(None);
    }
}

/// The model action an event step is.
fn event_action(step: Step) -> Option<&'static str> {
    match step {
        Step::BandRise => Some("BandRise"),
        Step::BandUnwant => Some("BandUnwant"),
        Step::PresRiseLease | Step::PresRiseStory => Some("PresRise"),
        Step::PresUnwant => Some("PresUnwant"),
        Step::PresRead => Some("PresRead"),
        Step::Cramp => Some("Cramp"),
        Step::At(_, a) => a,
        Step::AfterRise(_, a) => Some(a),
        Step::Wake => None,
    }
}

/// Every fall the model has: the two floored quiet falls and the two the
/// window's own shrink takes.
const FALLS: [&str; 4] = ["BandFall", "PresFall", "BandTaken", "PresTaken"];

/// The fixed build: every step the model's, every wake's decision the model's, every
/// projected state inside the invariants. Returns how many wakes re-gridded and how many
/// declined.
fn check_fixed(rig: &Rig) -> (usize, usize) {
    let (mut fell, mut declined) = (0, 0);
    for (i, (step, before, after)) in rig.trace.iter().enumerate() {
        for inv in &rig.model.invariants {
            assert!(
                rig.model.check_invariant(inv.name, after),
                "step {i} {step:?}: `{}` broken at {after:?}",
                inv.name
            );
        }
        match event_action(*step) {
            Some(action) => assert!(
                fires(&rig.model, action, before, after),
                "step {i} {step:?} is not `{action}`: {before:?} -> {after:?}"
            ),
            None if matches!(step, Step::At(..)) => assert_eq!(
                before, after,
                "step {i} {step:?}: time passed and moved what the model reads"
            ),
            None if before == after => {
                declined += 1;
                for fall in FALLS {
                    assert!(
                        !rig.model.action_enabled(fall, before),
                        "step {i}: the host declined `{fall}`, which the model allows at \
                         {before:?}"
                    );
                }
            }
            None => {
                fell += 1;
                let admitted = interp::admits(&rig.model, before, after);
                assert!(
                    admitted.is_some_and(|a| FALLS.contains(&a)),
                    "step {i}: the wake's re-grid is no fall the model admits ({admitted:?}): \
                     {before:?} -> {after:?}"
                );
            }
        }
    }
    (fell, declined)
}

/// The negative control: exactly one wake's re-grid is rejected by the committed model and
/// admitted at `Buggy = 1` only as `mutant`; every other step is admitted at `Buggy = 1`.
fn check_control(rig: &Rig, mutant: &str) {
    let mut caught = 0;
    for (i, (step, before, after)) in rig.trace.iter().enumerate() {
        if before == after {
            continue;
        }
        if interp::admits(&rig.model, before, after).is_none() {
            assert!(
                matches!(step, Step::Wake) && fires(&rig.buggy, mutant, before, after),
                "step {i} {step:?}: rejected, and not as `{mutant}`: {before:?} -> {after:?}"
            );
            caught += 1;
        } else {
            assert!(
                interp::admits(&rig.buggy, before, after).is_some(),
                "step {i} {step:?}"
            );
        }
    }
    assert_eq!(caught, 1, "the one quick fall of the schedule");
    assert!(
        rig.seen.quick,
        "the control re-gridded back inside the settle"
    );
}

/// A band row is up and dismissed; 5 ms before its quiet shrink the presence row is born.
/// The band waits the floor, and its deadline names it.
fn raise_then_bandshrink(rig: &mut Rig) {
    let ms = Duration::from_millis;
    let dropped = ms(1000);
    let shrink = dropped + SHRINK_QUIET;
    rig.run(&[
        Step::BandRise,
        Step::At(ms(500), Some("Tick")),
        Step::At(dropped, None),
        Step::BandUnwant,
        Step::At(shrink - ms(5), None),
        Step::PresRiseLease,
        Step::At(shrink, Some("BandQuiet")),
        Step::Wake,
    ]);
}

#[test]
fn a_presence_birth_just_before_the_bands_quiet_shrink_holds_it_to_the_floor() {
    crate::fabric::with_link_reset(|| {
        let mut rig = Rig::new(true);
        raise_then_bandshrink(&mut rig);
        assert_eq!(rig.app.message_band_rows, 1, "5 ms after the birth: held");
        let floor = rig.floor().expect("the birth noted a rise");
        assert_eq!(
            rig.app.messages_deadline(),
            Some(floor),
            "the band's wake is the floor"
        );
        rig.run(&[Step::AfterRise(CHROME_REGROW_SETTLE, "Tick"), Step::Wake]);
        assert_eq!(rig.app.message_band_rows, 0, "at the floor: the shrink");
        let (fell, declined) = check_fixed(&rig);
        assert_eq!((fell, declined), (1, 1), "{:#?}", rig.trace);

        // Negative control: no floor, so the shrink lands 5 ms after the birth.
        let mut control = Rig::new(false);
        raise_then_bandshrink(&mut control);
        assert_eq!(control.app.message_band_rows, 0);
        check_control(&control, "BandFallOwnQuiet");
    });
}

/// The turn's lease raised the row and ended; 12 ms before the row's timed fold a band row
/// rises. The fold waits the floor, and the presence deadline names it.
fn band_then_quietfold(rig: &mut Rig) {
    let ms = Duration::from_millis;
    let dropped = ms(500);
    let due = dropped + aterm_messages::presence::FOLD_QUIET;
    rig.run(&[
        Step::PresRiseLease,
        Step::At(dropped, Some("Tick")),
        Step::PresUnwant,
        Step::At(due - ms(12), None),
        Step::BandRise,
        Step::At(due, Some("PresQuiet")),
        Step::Wake,
    ]);
}

#[test]
fn a_band_rise_just_before_the_presence_rows_timed_fold_holds_it_to_the_floor() {
    crate::fabric::with_link_reset(|| {
        let mut rig = Rig::new(true);
        band_then_quietfold(&mut rig);
        assert_eq!(rig.app.presence_view(rig.wid).unwrap().rows, 1, "held");
        let floor = rig.floor().expect("the band's rise was noted");
        assert_eq!(
            rig.app.presence_deadline(rig.now),
            Some(floor),
            "the presence wake is the floor"
        );
        rig.run(&[Step::AfterRise(CHROME_REGROW_SETTLE, "Tick"), Step::Wake]);
        assert_eq!(rig.app.presence_view(rig.wid).unwrap().rows, 0, "folded");
        let (fell, declined) = check_fixed(&rig);
        assert_eq!((fell, declined), (1, 1), "{:#?}", rig.trace);

        let mut control = Rig::new(false);
        band_then_quietfold(&mut control);
        assert_eq!(control.app.presence_view(control.wid).unwrap().rows, 0);
        check_control(&control, "PresFallOwnQuiet");
    });
}

/// A told story's row has stood its quiet; a band row rises and 3 ms later a person's key
/// reads the story. The read is kept and folds at the floor.
fn band_then_fold_stood(rig: &mut Rig) {
    let ms = Duration::from_millis;
    rig.run(&[
        Step::PresRiseStory,
        Step::At(ms(2000), Some("Tick")),
        Step::BandRise,
        Step::At(ms(2003), None),
        Step::PresRead,
    ]);
}

#[test]
fn a_band_rise_just_before_a_persons_read_holds_the_read_to_the_floor() {
    crate::fabric::with_link_reset(|| {
        let mut rig = Rig::new(true);
        band_then_fold_stood(&mut rig);
        assert_eq!(rig.app.presence_view(rig.wid).unwrap().rows, 1, "held");
        assert!(rig.app.presence_view(rig.wid).unwrap().fold_read);
        rig.run(&[
            Step::Wake,
            Step::AfterRise(CHROME_REGROW_SETTLE, "Tick"),
            Step::Wake,
        ]);
        assert_eq!(rig.app.presence_view(rig.wid).unwrap().rows, 0, "folded");
        let (fell, declined) = check_fixed(&rig);
        assert_eq!((fell, declined), (1, 1), "{:#?}", rig.trace);

        // Negative control: the read folds at once, 3 ms after the rise (the read and an
        // unfloored fold in one step), which the committed model rejects and `Buggy = 1`
        // admits only as the read then `PresFallOwnQuiet`.
        let mut control = Rig::new(false);
        band_then_fold_stood(&mut control);
        assert_eq!(control.app.presence_view(control.wid).unwrap().rows, 0);
        let (_, before, after) = control.trace.last().expect("the read");
        assert!(interp::admits(&control.model, before, after).is_none());
        assert!(
            !fires_both(&control.model, "PresRead", "PresFall", before, after),
            "not the fixed law's read then fall"
        );
        assert!(
            fires_both(
                &control.buggy,
                "PresRead",
                "PresFallOwnQuiet",
                before,
                after
            ),
            "the read, then the unfloored fold: {before:?} -> {after:?}"
        );
        assert!(control.seen.quick);
    });
}

/// The window's own shrink, band row: a band row rises and is dismissed, and 100 ms later,
/// inside both its floor and its own quiet, the window loses its room. The window takes
/// the row at the next wake, neither quiet nor floor applying: `BandTaken`.
fn band_taken(rig: &mut Rig) {
    let ms = Duration::from_millis;
    rig.run(&[
        Step::BandRise,
        Step::At(ms(100), None),
        Step::BandUnwant,
        Step::At(ms(200), None),
        Step::Cramp,
        Step::Wake,
    ]);
}

/// The window's own shrink, presence row: the row is born and its want drops, and inside
/// the floor the window loses its room. The window takes the row at once: `PresTaken`.
fn pres_taken(rig: &mut Rig) {
    let ms = Duration::from_millis;
    rig.run(&[
        Step::PresRiseLease,
        Step::At(ms(100), None),
        Step::PresUnwant,
        Step::At(ms(200), None),
        Step::Cramp,
        Step::Wake,
    ]);
}

/// The last step of `rig`'s trace is the model's `action`, and it landed
/// inside the window's floor: the floor held nothing the window took.
fn took_inside_the_floor(rig: &Rig, action: &str) {
    let (step, before, after) = rig.trace.last().expect("the wake");
    assert!(matches!(step, Step::Wake), "{step:?}");
    assert!(
        fires(&rig.model, action, before, after),
        "the wake's re-grid is `{action}`: {before:?} -> {after:?}"
    );
    assert_eq!(before["since"], 0, "inside the floor: {before:?}");
    assert!(rig.floor().is_some_and(|f| rig.now < f));
    assert!(!rig.seen.quick, "a take is no unforced fall");
}

#[test]
fn a_window_that_loses_its_room_takes_the_bands_waiting_row_inside_the_floor() {
    crate::fabric::with_link_reset(|| {
        let mut rig = Rig::new(true);
        band_taken(&mut rig);
        assert_eq!(rig.app.message_band_rows, 0, "taken at once");
        took_inside_the_floor(&rig, "BandTaken");
        let (fell, declined) = check_fixed(&rig);
        assert_eq!((fell, declined), (1, 0), "{:#?}", rig.trace);
    });
}

#[test]
fn a_window_that_loses_its_room_takes_the_presence_rows_waiting_row_inside_the_floor() {
    crate::fabric::with_link_reset(|| {
        let mut rig = Rig::new(true);
        pres_taken(&mut rig);
        assert_eq!(rig.app.presence_view(rig.wid).unwrap().rows, 0, "taken");
        took_inside_the_floor(&rig, "PresTaken");
        let (fell, declined) = check_fixed(&rig);
        assert_eq!((fell, declined), (1, 0), "{:#?}", rig.trace);
    });
}

/// Two windows: the band re-grids every window, so its quiet shrink waits the latest floor
/// of every window with glass (`App::band_regrow_floor`). A band row is up and dismissed;
/// 5 ms before its quiet shrink a presence row is born in window 1, whose floor alone then
/// holds the band. `other_glass`: whether window 1 stands in for glass too.
fn two_windows(other_glass: bool) -> (Rig, WindowId) {
    let ms = Duration::from_millis;
    let mut rig = Rig::new(true);
    let first = rig.app.next_session_id;
    let other = rig
        .app
        .insert_logical_window(crate::stub_session(first), 24, 80);
    // A window's first stub session is not registered (no pty); a pushed tab is, so its
    // lease reaches the driver.
    let sid = rig.app.next_session_id;
    rig.app.push_stub_tab(other, crate::stub_session(sid));
    rig.app.frontmost_window = Some(rig.wid);
    rig.app
        .windows
        .get_mut(&other)
        .expect("window 1")
        .chrome_regrids_for_test = other_glass;
    let ctx = rig.app.pool.get(sid).expect("stub pooled").ctx.clone();
    let dropped = ms(1000);
    let shrink = dropped + SHRINK_QUIET;
    rig.run(&[
        Step::BandRise,
        Step::At(dropped, None),
        Step::BandUnwant,
        Step::At(shrink - ms(5), None),
    ]);
    *ctx.turn_lease.lock().unwrap() = Some(Lease::Turn {
        id: 7,
        driver: None,
        typing: false,
    });
    rig.app.on_presence_wake(&ctx.self_id, false);
    assert_eq!(
        rig.app.presence_view(other).unwrap().rows,
        1,
        "born in window 1"
    );
    assert_eq!(
        rig.app.presence_view(rig.wid).unwrap().rows,
        0,
        "not in window 0"
    );
    rig.run(&[Step::At(shrink, None), Step::Wake]);
    (rig, other)
}

/// A birth in window 1 holds the band's shrink to window 1's floor, and the band's wake
/// names it. Negative control: window 1 without glass has no floor, and the band shrinks
/// 5 ms after the birth, as a band floor that read only its own window would.
#[test]
fn a_presence_birth_in_another_window_holds_the_bands_shrink_to_that_windows_floor() {
    crate::fabric::with_link_reset(|| {
        let (mut rig, other) = two_windows(true);
        let floor = rig
            .app
            .chrome_regrow_floor(other)
            .expect("the birth noted a rise on window 1");
        assert!(
            rig.app
                .chrome_regrow_floor(rig.wid)
                .is_some_and(|own| own < rig.now),
            "window 0's own floor (the band's rise) has long run"
        );
        assert_eq!(rig.app.band_regrow_floor(), Some(floor), "the latest");
        assert_eq!(rig.app.message_band_rows, 1, "5 ms after the birth: held");
        assert_eq!(
            rig.app.messages_deadline(),
            Some(floor),
            "the band's wake is window 1's floor"
        );
        rig.set_clock(floor - Duration::from_millis(1));
        rig.wake();
        assert_eq!(rig.app.message_band_rows, 1);
        rig.set_clock(floor);
        rig.wake();
        assert_eq!(rig.app.message_band_rows, 0, "at window 1's floor");

        let (control, other) = two_windows(false);
        assert_eq!(control.app.chrome_regrow_floor(other), None);
        assert_eq!(
            control.app.message_band_rows, 0,
            "no floor on window 1: the band shrank 5 ms after the birth there"
        );
    });
}

/// A window that never re-grids for its chrome (headless, no stand-in) has no floor; one
/// that does reads the latest rise of any source.
#[test]
fn only_a_window_whose_grid_moves_for_its_chrome_has_a_floor() {
    crate::fabric::with_link_reset(|| {
        let mut rig = Rig::new(false);
        rig.step(Step::BandRise);
        assert!(rig.app.windows[&rig.wid].regrow_floor.last_rise().is_some());
        assert_eq!(rig.app.chrome_regrow_floor(rig.wid), None);
        assert_eq!(rig.app.band_regrow_floor(), None);
        let mut glass = Rig::new(true);
        glass.step(Step::BandRise);
        let floor = glass.floor();
        assert!(floor.is_some());
        assert_eq!(glass.app.chrome_regrow_floor(glass.wid), floor);
        assert_eq!(glass.app.band_regrow_floor(), floor);
    });
}

// The machine's other actions: environment steps, time, the frame and the `Buggy`
// members. The re-grids carry `#[refines]` anchors on `App::regrid_message_band` and
// `App::regrid_window_for_chrome_rows`.
#[aterm_spec::spec_unmodeled(
    machine = "ChromeRegrowFloor",
    action = "BandUnwant",
    reason = "The band row's want drops (a dismissal, a hold's end): the center's state. \
              Tier-1 dismisses a real row and settles the real host step."
)]
#[aterm_spec::spec_unmodeled(
    machine = "ChromeRegrowFloor",
    action = "BandBack",
    reason = "A band row wanted again while its shrink waits: no re-grid (D1). The engine's \
              `commit_rows` Equal arm; pinned by the center's own D1 tests."
)]
#[aterm_spec::spec_unmodeled(
    machine = "ChromeRegrowFloor",
    action = "PresUnwant",
    reason = "The presence row's want drops (a lease ends): the session's facts. Tier-1 ends a \
              real turn lease and delivers its wake."
)]
#[aterm_spec::spec_unmodeled(
    machine = "ChromeRegrowFloor",
    action = "PresBack",
    reason = "The presence row wanted again while its fold waits: no re-grid (ruling 394). \
              Pinned by `presence_drive_tests`' fold-quiet tests."
)]
#[aterm_spec::spec_unmodeled(
    machine = "ChromeRegrowFloor",
    action = "PresRead",
    reason = "A person's keystroke reads a calm story: the fold law's input. Tier-1 drives the \
              test-only `App::note_human_acted`, a seam onto the real `drive::human_acted`; \
              the shipping key arm (`App::dispatch_hw_key`) reads through \
              `drive::human_acted_on` after `on_key`."
)]
#[aterm_spec::spec_unmodeled(
    machine = "ChromeRegrowFloor",
    action = "BandQuiet",
    reason = "Time: the band's own `SHRINK_QUIET` runs out. Tier-1 steps the clock past \
              `MessageCenter::quiet_shrink_at`."
)]
#[aterm_spec::spec_unmodeled(
    machine = "ChromeRegrowFloor",
    action = "PresQuiet",
    reason = "Time: the presence row's own `FOLD_QUIET` runs out. Tier-1 steps the clock past \
              `presence::drive::fold_due`."
)]
#[aterm_spec::spec_unmodeled(
    machine = "ChromeRegrowFloor",
    action = "Tick",
    reason = "Time: the floor's `CHROME_REGROW_SETTLE` runs out. Tier-1 steps the clock past \
              the window's `RegrowFloor::floor`."
)]
#[aterm_spec::spec_unmodeled(
    machine = "ChromeRegrowFloor",
    action = "Cramp",
    reason = "The window's frame loses its room (a drag, a zoom): the OS window's size, \
              booked by `on_resize`. Tier-1 takes the stand-in's terminal rows to none and \
              drives both takes (`BandTaken`, `PresTaken`) inside the floor."
)]
#[aterm_spec::spec_unmodeled(
    machine = "ChromeRegrowFloor",
    action = "Uncramp",
    reason = "The window's frame gets its room back: the OS window's size."
)]
#[aterm_spec::spec_unmodeled(
    machine = "ChromeRegrowFloor",
    action = "BandFallOwnQuiet",
    reason = "`Buggy = 1` mutant: the band's quiet shrink before the floor. The Tier-1 \
              negative control is admitted only as this."
)]
#[aterm_spec::spec_unmodeled(
    machine = "ChromeRegrowFloor",
    action = "PresFallOwnQuiet",
    reason = "`Buggy = 1` mutant: the presence fold before the floor. The Tier-1 negative \
              control is admitted only as this."
)]
#[aterm_spec::spec_unmodeled(
    machine = "ChromeRegrowFloor",
    action = "Restart",
    reason = "`Buggy = 1` mutant: a floor re-anchored on a step that is no rise. No shipping \
              code re-anchors: `RegrowFloor::rose` alone moves it, pinned by the engine's \
              `chrome` tests."
)]
#[expect(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it"
)]
fn chrome_regrow_waivers() {}
