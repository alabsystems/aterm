// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 bind for `ChromeRegrowFloor`
//! (`aterm_models_gui::chrome_regrow_floor_model`, ruling 414) —
//! after any rise of a window's chrome rows, neither the band's quiet fall nor
//! the tab strip's fall re-grids the window back inside
//! `CHROME_REGROW_SETTLE`.
//!
//! WHAT WAS MEASURED (2026-09-29, main `8b24e99a4`, real windows, a stand-in
//! that reads its size 20 ms after the `SIGWINCH` and ticks a spinner every
//! 5 ms): a chrome row rising milliseconds before the band's quiet shrink
//! shifted the alternate screen (a net-zero shrink-then-grow pair). The
//! presence row was that other source then; presence owns no chrome row since
//! the owner surfaces (2026-10-16), and the TAB STRIP is the one other row a
//! window's grid gives way to. On 2026-10-19 the windowed lane
//! (`aterm-lab presence-flap --window --phases X`) measured the strip's fall —
//! a Settings edit, then immediate and unfloored — re-gridding a window back
//! 38 ms after a band row's rise; the strip's fall waits the floor since.
//!
//! THE RIG. Everything shipping is REAL: the band through the center
//! (`MessageCenter::post`, `dismiss`) and the host's step
//! (`App::settle_messages`: the band's commit with the floor the host hands,
//! the re-grid, the rise noted on every window); the strip through
//! `App::set_tab_strip_rows`, the seam every config load and reload takes, and
//! its waiting fall through `App::settle_strip_fall`, run by the host's step;
//! and the loop's wake (`App::settle_messages`, `App::sync_message_band_rows`).
//! The window is a headless one standing in for glass
//! (`chrome_regrids_for_test`, which every glass test of the host reads
//! through `app_presence::chrome_regrids_grid`: the floor, and the band's
//! afford law), and the rig steps ONE clock, so every instant is the rig's.
//! The window's own shrink is the stand-in's terminal rows taken to none
//! ([`Step::Cramp`]), which a real window's `on_resize` books after a drag or
//! a zoom.
//!
//! The schedules: the two measured ones (each with its negative control), an
//! edit back while the strip's fall waits (`StripBack`), an edit down outside
//! the floor (taken at once), and the window's own shrink taking the band's
//! waiting row inside the floor (`BandTaken`).
//!
//! Each step is one of the model's: an EVENT (a source's want or rise), a
//! passage of TIME with no host code run (a tick of the floor, the band's own
//! quiet), or the loop's WAKE. After each, the host is projected onto the
//! model's variables ([`project`]) and checked BY NAME: an event or a passage
//! of time must be the model's action it is; a wake that re-grids must be a
//! fall the model admits; a wake that declines must decline exactly where
//! the model enables no fall. The host's armed deadline must name the floor.
//!
//! Projection: `bw`/`b` the band's want (`MessageCenter::wanted_rows`) and
//! reserved rows (`message_band_rows`); `sw`/`s` the rows the Settings ask for
//! the strip (a waiting fall's, else the strip's) and the strip's rows; `bq`
//! whether a
//! pending fall's OWN due has passed (`MessageCenter::quiet_shrink_at`);
//! `since` the ENGINE's count, off the window's `RegrowFloor`; and — ground
//! truth, from what the rig SAW — `age` (the chrome total's last rise),
//! `quick` (an unforced fall inside the settle of it), `cramped` (the rig
//! took the window's room) and `flips`.
//!
//! NEGATIVE CONTROLS: each measured schedule on a window with no floor (a
//! headless window not standing in for glass: the law before the floor, a fall
//! once its own cause ran) re-grids back inside the settle, which the
//! committed model rejects and `Buggy = 1` admits only as the law that shipped
//! — `BandFallOwnQuiet` for the band, `StripFallAtOnce` for the strip.

use std::time::{Duration, Instant};

use aterm_messages::chrome::CHROME_REGROW_SETTLE;
use aterm_messages::{Message, MessageId, SHRINK_QUIET, Severity, WallStamp, tags};
use aterm_models_gui::chrome_regrow_floor_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};

use crate::{App, WindowId};

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
    let bw = app.messages.wanted_rows() > 0;
    let b = app.message_band_rows > 0;
    // The band's OWN quiet reads as run unless a fall of it is pending and
    // its own due is still ahead.
    let bq = !(b && !bw) || app.messages.quiet_shrink_at().is_some_and(|x| now >= x);
    let young = |at: Option<Instant>| at.is_some_and(|x| now < x);
    let strip = app.tab_strip_rows > 0;
    let mut s = State::new();
    s.insert("bw", i64::from(bw));
    s.insert("b", i64::from(b));
    s.insert(
        "sw",
        i64::from(app.strip_fall.map_or(strip, |rows| rows > 0)),
    );
    s.insert("s", i64::from(strip));
    s.insert("bq", i64::from(bq));
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

/// One step of a schedule.
#[derive(Clone, Copy, Debug)]
enum Step {
    /// A band row reaches the glass (posted and settled).
    BandRise,
    /// The band row is dismissed (its want drops; its quiet starts).
    BandUnwant,
    /// The tab strip grows to one row (`App::set_tab_strip_rows`).
    StripRise,
    /// A Settings edit takes the strip to none (`App::set_tab_strip_rows`).
    StripUnwant,
    /// A Settings edit gives the strip its row back while its fall waits.
    StripBack,
    /// The window's frame loses its room: the stand-in's terminal rows drop
    /// to none, so its grid can afford no band row (the drag or zoom a real
    /// window's `on_resize` books). The afford law reads the stand-in as
    /// glass (`app_presence::chrome_regrids_grid`).
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

    /// The loop's wake at the rig's instant: the band's step, and the park's
    /// convergence of the band's count.
    fn wake(&mut self) {
        let now = self.now;
        let _ = self.app.settle_messages(now);
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
            Step::StripRise | Step::StripBack => {
                self.app.set_tab_strip_rows(1, self.now);
                self.seen.flips += 1;
            }
            Step::StripUnwant => {
                self.app.set_tab_strip_rows(0, self.now);
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

/// The model action an event step is.
fn event_action(step: Step) -> Option<&'static str> {
    match step {
        Step::BandRise => Some("BandRise"),
        Step::BandUnwant => Some("BandUnwant"),
        Step::StripRise => Some("StripRise"),
        Step::StripUnwant => Some("StripUnwant"),
        Step::StripBack => Some("StripBack"),
        Step::Cramp => Some("Cramp"),
        Step::At(_, a) => a,
        Step::AfterRise(_, a) => Some(a),
        Step::Wake => None,
    }
}

/// Every fall the model has: the floored quiet fall, the one the window's own
/// shrink takes, and the strip's floored fall.
const FALLS: [&str; 3] = ["BandFall", "BandTaken", "StripFall"];

/// Whether event `action` takes `before` to `after` in `model` — alone, or with
/// the fall the same host call takes when nothing holds it (a Settings edit down
/// outside the floor is `StripUnwant` then `fall`).
fn fires_event(model: &Model, action: &str, fall: &str, before: &State, after: &State) -> bool {
    fires(model, action, before, after)
        || (action == "StripUnwant"
            && model
                .successors(action, before)
                .iter()
                .any(|mid| fires(model, fall, mid, after)))
}

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
                fires_event(&rig.model, action, "StripFall", before, after),
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

/// The negative control: exactly one re-grid is rejected by the committed model and
/// admitted at `Buggy = 1` only as `mutant` (a wake's, or an edit's with its fall in the
/// same call); every other step is admitted at `Buggy = 1`.
fn check_control(rig: &Rig, mutant: &str) {
    let mut caught = 0;
    for (i, (step, before, after)) in rig.trace.iter().enumerate() {
        if before == after {
            continue;
        }
        if interp::admits(&rig.model, before, after).is_none() {
            let as_mutant = match event_action(*step) {
                None => matches!(step, Step::Wake) && fires(&rig.buggy, mutant, before, after),
                Some(action) => fires_event(&rig.buggy, action, mutant, before, after),
            };
            assert!(
                as_mutant,
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

/// RISE THEN BANDSHRINK: a band row is up and dismissed; 5 ms before its
/// quiet shrink the strip grows. The band waits the floor, and its state
/// deadline names it.
fn rise_then_bandshrink(rig: &mut Rig) {
    let ms = Duration::from_millis;
    let dropped = ms(1000);
    let shrink = dropped + SHRINK_QUIET;
    rig.run(&[
        Step::BandRise,
        Step::At(ms(500), Some("Tick")),
        Step::At(dropped, None),
        Step::BandUnwant,
        Step::At(shrink - ms(5), None),
        Step::StripRise,
        Step::At(shrink, Some("BandQuiet")),
        Step::Wake,
    ]);
}

#[test]
fn a_strip_rise_just_before_the_bands_quiet_shrink_holds_it_to_the_floor() {
    crate::fabric::with_link_reset(|| {
        let mut rig = Rig::new(true);
        rise_then_bandshrink(&mut rig);
        assert_eq!(rig.app.message_band_rows, 1, "5 ms after the rise: held");
        let floor = rig.floor().expect("the rise was noted");
        assert_eq!(
            rig.app.messages_deadline(),
            Some(floor),
            "the band's wake is the floor"
        );
        rig.run(&[Step::AfterRise(CHROME_REGROW_SETTLE, "Tick"), Step::Wake]);
        assert_eq!(rig.app.message_band_rows, 0, "at the floor: the shrink");
        let (fell, declined) = check_fixed(&rig);
        assert_eq!((fell, declined), (1, 1), "{:#?}", rig.trace);

        // NEGATIVE CONTROL: no floor — the shrink lands 5 ms after the rise.
        let mut control = Rig::new(false);
        rise_then_bandshrink(&mut control);
        assert_eq!(control.app.message_band_rows, 0);
        check_control(&control, "BandFallOwnQuiet");
    });
}

/// BAND RISE THEN STRIPFALL (the pair measured on 2026-10-19, 38 ms apart):
/// the strip is up and long settled; a band row rises, and 38 ms later a
/// Settings edit takes the strip down. The strip's fall waits the floor, and
/// the host's wake names it.
fn band_rise_then_stripfall(rig: &mut Rig) {
    let ms = Duration::from_millis;
    let rise = ms(2000);
    rig.run(&[
        Step::StripRise,
        Step::At(ms(500), Some("Tick")),
        Step::At(rise, None),
        Step::BandRise,
        Step::At(rise + ms(38), None),
        Step::StripUnwant,
        Step::Wake,
    ]);
}

#[test]
fn a_settings_edit_just_after_a_band_rise_holds_the_strips_fall_to_the_floor() {
    crate::fabric::with_link_reset(|| {
        let mut rig = Rig::new(true);
        band_rise_then_stripfall(&mut rig);
        assert_eq!(rig.app.tab_strip_rows, 1, "38 ms after the rise: held");
        let floor = rig.floor().expect("the rise was noted");
        assert_eq!(
            rig.app.strip_fall_due(),
            Some(floor),
            "the strip's wake is the floor"
        );
        assert!(
            rig.app.messages_deadline().is_some_and(|d| d <= floor),
            "the loop wakes by the floor: {:?}",
            rig.app.messages_deadline()
        );
        rig.run(&[Step::AfterRise(CHROME_REGROW_SETTLE, "Tick"), Step::Wake]);
        assert_eq!(rig.app.tab_strip_rows, 0, "at the floor: the fall");
        assert_eq!(rig.app.strip_fall_due(), None, "no fall left waiting");
        let (fell, declined) = check_fixed(&rig);
        assert_eq!((fell, declined), (1, 1), "{:#?}", rig.trace);

        // NEGATIVE CONTROL: no floor — the edit re-grids 38 ms after the rise.
        let mut control = Rig::new(false);
        band_rise_then_stripfall(&mut control);
        assert_eq!(control.app.tab_strip_rows, 0);
        check_control(&control, "StripFallAtOnce");
    });
}

/// AN EDIT BACK while the strip's fall waits: the strip keeps its row, no
/// wake re-grids, and no fall is left waiting.
#[test]
fn a_settings_edit_back_while_the_strips_fall_waits_regrids_nothing() {
    crate::fabric::with_link_reset(|| {
        let ms = Duration::from_millis;
        let mut rig = Rig::new(true);
        band_rise_then_stripfall(&mut rig);
        rig.run(&[
            Step::At(ms(2100), None),
            Step::StripBack,
            Step::Wake,
            Step::AfterRise(CHROME_REGROW_SETTLE, "Tick"),
            Step::Wake,
        ]);
        assert_eq!(rig.app.tab_strip_rows, 1);
        assert_eq!(rig.app.strip_fall_due(), None);
        let (fell, declined) = check_fixed(&rig);
        assert_eq!((fell, declined), (0, 3), "{:#?}", rig.trace);
    });
}

/// AN EDIT DOWN OUTSIDE THE FLOOR: the strip's fall is taken in the edit's own
/// call (`StripUnwant`, then `StripFall`), with nothing left to wake for.
#[test]
fn a_settings_edit_down_outside_the_floor_takes_the_strip_at_once() {
    crate::fabric::with_link_reset(|| {
        let ms = Duration::from_millis;
        let mut rig = Rig::new(true);
        rig.run(&[
            Step::StripRise,
            Step::At(ms(500), Some("Tick")),
            Step::StripUnwant,
        ]);
        assert_eq!(rig.app.tab_strip_rows, 0);
        assert_eq!(rig.app.strip_fall_due(), None);
        let (fell, declined) = check_fixed(&rig);
        assert_eq!((fell, declined), (0, 0), "{:#?}", rig.trace);
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
        // The strip's rise is a rise on that floor too.
        glass.set_clock(glass.now + Duration::from_secs(5));
        glass.step(Step::StripRise);
        assert!(glass.floor() > floor, "the strip's rise moved the anchor");
    });
}
