// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE KEPT CHIP BLOCKS ([`WindowLights::lend_block`]): a pane's block is
//! fitted once per [`FitKey`] and LENT to every frame after it while the key
//! stands — and the key holds every input the fit reads (the pane's ground,
//! the accent, the keyboard's and the pointer's chips, the room, Autopilot's
//! title as its reading built it, and the session's lights: Claude's mode,
//! a toggle in flight, a refusal (its light and its reason) or a notice
//! while on show, the Autopilot word, a click in flight and the harness's
//! glass), so a frame whose inputs moved fits anew and draws what fitting
//! anew draws. Here input by input on the window's lights, and over a
//! scripted run of real frames against a window that keeps nothing and
//! reads every row.

use std::ops::Range;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use aterm_agent::harness::acts::{Act, TaskFace};
use aterm_agent::harness::lights::{Light, Mode, Reading};
use aterm_agent::supervise::Autopilot;

use super::gesture_tests::{BYPASS, Hosted, app, draw_claude, frame, host, publish_facts};
use super::*;

/// The session the window's lights hold.
const S: u64 = 7;
/// The footer's mark accent, as a theme gives it.
const ACCENT: [u8; 3] = [0, 128, 255];

fn blank() -> RenderCell {
    RenderCell {
        ch: ' ',
        fg: [200, 200, 200],
        bg: [0, 0, 0],
        ..RenderCell::default()
    }
}

/// An instant an hour on: a refusal or a notice on show.
fn later() -> Instant {
    Instant::now() + Duration::from_secs(3600)
}

/// An instant gone: a refusal or a notice whose show ended.
fn gone() -> Instant {
    Instant::now()
        .checked_sub(Duration::from_secs(1))
        .expect("an instant before now")
}

/// What a block lays, cell for cell — its fit number aside.
#[derive(Debug, Clone, PartialEq)]
struct Laid {
    title: Vec<RenderCell>,
    lean: [Vec<RenderCell>; LEAN_FORMS],
    short_title: Vec<RenderCell>,
    reason: bool,
    lights: Vec<RenderCell>,
    cols: Vec<(Range<usize>, Chip)>,
    holds: usize,
}

impl Laid {
    fn of(b: &Block) -> Self {
        Self {
            title: b.title.clone(),
            lean: b.lean.clone(),
            short_title: b.short_title.clone(),
            reason: b.reason,
            lights: b.lights.clone(),
            cols: b.cols.clone(),
            holds: b.holds,
        }
    }
}

/// One pane's frame: the block lent (as it lays), the fits it took, and
/// what fitting anew lays from the same inputs.
#[derive(Debug)]
struct Framed {
    lent: Option<Laid>,
    fits: usize,
    anew: Option<Laid>,
}

/// A window's lights holding session [`S`], and the fit's own inputs.
struct World {
    w: WindowLights,
    blank: RenderCell,
    accent: [u8; 3],
    /// The keyboard's and the pointer's chips.
    focus: (Option<Chip>, Option<Chip>),
    room: usize,
    /// Autopilot's title is read ([`World::title`]).
    titled: bool,
}

impl World {
    /// [`S`] under Cruise with v1's glass, Claude in bypass, a wide rule.
    fn cruise() -> Self {
        let mut w = WindowLights::default();
        w.note_autopilot_at(
            S,
            None,
            AutopilotView::Cruise,
            ChipGlass::default(),
            Instant::now,
        );
        w.sessions.get_mut(&S).expect("noted").shown = Reading {
            mode: Some(Mode::Bypass),
            pill: true,
        };
        Self {
            w,
            blank: blank(),
            accent: ACCENT,
            focus: (None, None),
            room: 80,
            titled: false,
        }
    }

    fn session(&mut self) -> &mut SessionLights {
        self.w.sessions.get_mut(&S).expect("the session's lights")
    }

    /// Autopilot's title as a reading builds it ([`App::claude_lights_block`]):
    /// `whole`, numbered from the window's fits, under the pointer.
    fn title(&mut self, whole: &str) {
        self.w.hover = Some((S, Target::from(Chip::Autopilot)));
        self.focus.1 = Some(Chip::Autopilot);
        self.titled = true;
        self.w.fits += 1;
        let reading = TitleReading {
            session: S,
            generation: 0,
            trail: String::new(),
            today: None,
            built: (AutopilotView::Cruise, ChipGlass::default()),
            whole: whole.to_owned(),
            short: "autopilot: cruise".to_owned(),
            laid: self.w.fits,
        };
        match self.w.readings.iter_mut().find(|r| r.session == S) {
            Some(r) => *r = reading,
            None => self.w.readings.push(reading),
        }
    }

    /// The selection and the hover, as the lights hold them.
    fn targets(&self) -> (Option<Target>, Option<Target>) {
        (
            self.focus.0.map(Target::Chip),
            self.focus.1.map(Target::Chip),
        )
    }

    fn at(&self) -> Option<usize> {
        self.titled
            .then(|| self.w.readings.iter().position(|r| r.session == S))
            .flatten()
    }

    /// What fitting anew lays for the inputs as they stand.
    fn anew(&self) -> Option<Laid> {
        let at = self.at();
        let autopilot = at.map(|i| {
            let r = &self.w.readings[i];
            (r.whole.as_str(), r.short.as_str())
        });
        let mut block = Block::default();
        self.w
            .fit_into(
                S,
                self.blank,
                Instant::now(),
                self.accent,
                self.focus.0,
                self.focus.1,
                self.room,
                (autopilot, None, None),
                &mut block,
            )
            .then(|| Laid::of(&block))
    }

    /// One frame drawing `panes` (leaf indices) of [`S`]: each pane's block
    /// lent and handed back once painted, as the footer's pass does.
    fn frame_of(&mut self, panes: &[usize]) -> Vec<Framed> {
        self.w.clear_hits();
        self.lend_each(panes)
    }

    /// [`Self::frame_of`] past the frame's start ([`WindowLights::clear_hits`]).
    fn lend_each(&mut self, panes: &[usize]) -> Vec<Framed> {
        let at = self.at();
        panes
            .iter()
            .map(|&pane| {
                let anew = self.anew();
                let _ = crate::work_counts::take_footer_block_fits();
                let block = self.w.lend_block(
                    S,
                    pane,
                    self.blank,
                    self.accent,
                    self.targets(),
                    self.room,
                    at.map(TitleAt::Autopilot),
                );
                let fits = crate::work_counts::take_footer_block_fits();
                let lent = block.as_ref().map(Laid::of);
                if let Some(block) = block {
                    self.w.give_back(block);
                }
                Framed { lent, fits, anew }
            })
            .collect()
    }

    fn frame(&mut self) -> Framed {
        self.frame_of(&[0]).pop().expect("one pane")
    }
}

/// A toggle of the mode light in flight.
fn pending() -> Pending {
    Pending {
        light: Light::Mode,
        started: Mode::Plan,
        mode_at_press: Mode::Plan,
        seen: Vec::new(),
        presses: 1,
        deadline: later(),
        next_poll: None,
        held_mid_turn: false,
    }
}

fn nothing(_: &mut World) {}

/// Claude in plan mode: the light, a deviation, drawn beside Autopilot.
fn planned(w: &mut World) {
    w.session().shown = Reading {
        mode: Some(Mode::Plan),
        pill: true,
    };
}

fn land(w: &mut World) {
    w.session().autopilot = Some(AutopilotView::Land);
}

/// Cruise's title as `autopilot_title` composes it with a trail of `last`,
/// its explanation read from the one table, so the fit is fed the length
/// and the shape the app draws.
fn cruise_title(last: &str) -> String {
    format!(
        "autopilot: cruise \u{2014} {} \u{b7} last: {last}",
        crate::autopilot::mode_words(Autopilot::Cruise).chip
    )
}

fn titled(w: &mut World) {
    w.title(&cruise_title("\u{2713} approved 14:05"));
}

fn refusing(w: &mut World) {
    w.session().refused = Some((Light::Mode, Refusal::NotShown, later()));
}

fn noticing(w: &mut World) {
    w.session().notice = Some((Light::Mode, later()));
}

fn lit(w: &mut World) {
    w.session().glass.lit = Some(Act::Approved);
}

/// One input of the fit: its name, the state it moves from, and its move.
type Case = (&'static str, fn(&mut World), fn(&mut World));

/// Every input the fit reads, each moved in a way the glass shows.
fn cases() -> Vec<Case> {
    vec![
        ("the pane's ground", nothing, |w| w.blank.bg = [10, 10, 40]),
        ("the theme's accent", nothing, |w| w.accent = [255, 0, 128]),
        ("the keyboard's chip", nothing, |w| {
            w.focus.0 = Some(Chip::Autopilot);
        }),
        ("the pointer's chip", nothing, |w| {
            w.focus.1 = Some(Chip::Autopilot);
        }),
        ("the room", planned, |w| w.room = 2),
        ("Autopilot's title built anew", titled, |w| {
            w.title(&cruise_title("\u{21bb} kept going 14:07"));
        }),
        ("Claude's mode", nothing, |w| {
            w.session().shown = Reading {
                mode: Some(Mode::Plan),
                pill: true,
            };
        }),
        ("a toggle in flight", nothing, |w| {
            w.session().pending = Some(pending());
        }),
        ("a refusal on show", nothing, refusing),
        ("a refusal's show ending", refusing, |w| {
            w.session().refused.as_mut().expect("refused").2 = gone();
        }),
        // A second click refused for another reason while the first still
        // shows, on the same light: the title says the new reason.
        ("a refusal's new reason", refusing, |w| {
            w.session().refused = Some((Light::Mode, Refusal::StoppedMidTurn(Mode::Plan), later()));
        }),
        ("a notice on show", nothing, noticing),
        ("a notice's show ending", noticing, |w| {
            w.session().notice.as_mut().expect("noticed").1 = gone();
        }),
        ("Autopilot's word", nothing, land),
        ("an Autopilot click in flight", nothing, |w| {
            w.session().autopilot_pending = Some((Autopilot::Land, later()));
        }),
        ("an act lit", nothing, lit),
        ("an act's dwell out", lit, |w| w.session().glass.lit = None),
        ("a timer armed", nothing, |w| {
            w.session().glass.land_at = Some(LandAtGlass {
                clock: Clock::of_minute(18 * 60),
                other_day: None,
            });
        }),
        ("the task's face", land, |w| {
            let glass = &mut w.session().glass;
            glass.task = Some(TaskFace::Landed);
            glass.task_at = Some(Stamp {
                clock: Clock::of_minute(17 * 60 + 42),
                day: 20_378,
            });
        }),
        ("the host gone", nothing, |w| {
            w.session().autopilot = Some(AutopilotView::Unhosted);
        }),
    ]
}

/// A BLOCK IS KEPT WHILE ITS KEY STANDS, AND FITTED ANEW WHERE ANY INPUT
/// MOVES: for every input the fit reads, a pane's first frame fits its
/// block, an unchanged frame lends the kept one (no fit) laying what
/// fitting anew lays; the input moved, the next frame fits anew (the key
/// missed) and lays what fitting anew lays — not what it laid before (the
/// input moves the glass) — and the frame after keeps that.
///
/// NEGATIVE CONTROL: the first frame of each fits (the counter is wired),
/// and every moved input is seen by the glass.
#[test]
fn a_kept_block_is_lent_while_its_key_stands_and_fitted_anew_where_any_input_moves() {
    for (what, from, moved) in cases() {
        let mut world = World::cruise();
        from(&mut world);
        let first = world.frame();
        assert_eq!(first.fits, 1, "{what}: CONTROL: the first frame fits");
        assert_eq!(first.lent, first.anew, "{what}: the first fit");
        let kept = world.frame();
        assert_eq!(kept.fits, 0, "{what}: an unchanged frame keeps its block");
        assert_eq!(kept.lent, kept.anew, "{what}: the kept block is the fit's");
        assert_eq!(kept.lent, first.lent, "{what}: the same block");
        moved(&mut world);
        let now = world.frame();
        assert_eq!(now.fits, 1, "{what}: the moved input misses");
        assert_eq!(
            now.lent, now.anew,
            "{what}: and lays what fitting anew lays"
        );
        assert_ne!(now.lent, first.lent, "{what}: CONTROL: the glass moved");
        let again = world.frame();
        assert_eq!(
            (again.fits, &again.lent),
            (0, &now.lent),
            "{what}: kept at the key it moved to"
        );
    }
}

/// A KEPT BLOCK COSTS A FRAME NOTHING, AND EACH PANE KEEPS ITS OWN: two
/// panes of one session fit a block each, and the frames after lend them
/// both with no fit and no allocation; a pane the frame does not draw lets
/// its block go to the spares (no allocation) and the pane drawn again
/// fits anew. A block lent twice in one frame (not yet handed back) is laid
/// anew the second time, the same cells.
///
/// NEGATIVE CONTROL: with nothing kept (`fit_anew_in`) every frame fits.
#[test]
fn a_kept_block_costs_a_frame_nothing_and_each_pane_keeps_its_own() {
    let mut world = World::cruise();
    let first = world.frame_of(&[0, 1]);
    assert_eq!(first.iter().map(|f| f.fits).sum::<usize>(), 2, "one each");
    for i in 0..5 {
        let (frame, allocations) = crate::work_counts::allocations_in(|| world.frame_of(&[0, 1]));
        assert!(frame.iter().all(|f| f.fits == 0), "frame {i}: {frame:?}");
        assert!(frame.iter().all(|f| f.lent == f.anew), "frame {i}");
        // The anew fits of the comparison allocate; the lends do not.
        let lends = crate::work_counts::allocations_in(|| {
            world.w.clear_hits();
            for pane in [0, 1] {
                let block = world.w.lend_block(
                    S,
                    pane,
                    world.blank,
                    world.accent,
                    world.targets(),
                    world.room,
                    None,
                );
                world.w.give_back(block.expect("drawn"));
            }
        })
        .1;
        assert_eq!(lends, 0, "frame {i}: the lends allocate nothing");
        assert!(allocations > 0, "CONTROL: the comparison's fits allocate");
    }
    let one = world.frame_of(&[0]);
    assert_eq!(one[0].fits, 0, "pane 0 still kept");
    let spares = world.w.spare_blocks.len();
    let (_, let_go) = crate::work_counts::allocations_in(|| world.w.clear_hits());
    assert_eq!(let_go, 0, "a block let go allocates nothing");
    assert_eq!(world.w.fitted.len(), 1, "pane 1's block let go");
    assert_eq!(world.w.spare_blocks.len(), spares + 1, "to the spares");
    let back = world.lend_each(&[0, 1]);
    assert_eq!(
        back.iter().map(|f| f.fits).collect::<Vec<_>>(),
        [0, 1],
        "pane 1 fits anew"
    );
    // Lent twice in one frame.
    world.w.clear_hits();
    let _ = crate::work_counts::take_footer_block_fits();
    let a = world
        .w
        .lend_block(
            S,
            0,
            world.blank,
            world.accent,
            world.targets(),
            world.room,
            None,
        )
        .expect("drawn");
    let b = world
        .w
        .lend_block(
            S,
            0,
            world.blank,
            world.accent,
            world.targets(),
            world.room,
            None,
        )
        .expect("drawn");
    assert_eq!(
        crate::work_counts::take_footer_block_fits(),
        1,
        "the second laid anew"
    );
    assert_eq!(Laid::of(&a), Laid::of(&b), "the same cells");
    world.w.give_back(a);
    world.w.give_back(b);
    // CONTROL: nothing kept.
    fit_anew_in(|| {
        for i in 0..3 {
            let frame = world.frame_of(&[0, 1]);
            assert!(frame.iter().all(|f| f.fits == 1), "anew {i}: {frame:?}");
            assert!(frame.iter().all(|f| f.lent == f.anew), "anew {i}");
        }
    });
}

/// Every rebuilt face reads the day its title is built on, including a
/// historical task first restated under a reading that began without one.
/// Unchanged readings never call the event provider. The provider is the
/// same production-used refresh seam, supplied captured local data here.
#[test]
fn a_title_rebuild_reads_its_day_for_each_changed_face_and_not_for_a_kept_one() {
    use std::cell::Cell;
    const FRI: i64 = 20_007;
    const TODAY: i64 = FRI + 1;
    let stamp = |minute| Stamp {
        clock: Clock::of_minute(minute),
        day: FRI,
    };
    let built = |view, task, task_at| {
        (
            view,
            ChipGlass {
                task,
                task_at,
                ..ChipGlass::default()
            },
        )
    };
    let empty = built(AutopilotView::Land, None, None);
    let landed = built(
        AutopilotView::Land,
        Some(TaskFace::Landed),
        Some(stamp(17 * 60 + 42)),
    );
    let theirs = built(
        AutopilotView::Land,
        Some(TaskFace::Theirs),
        Some(stamp(17 * 60 + 42)),
    );
    let make = |built: (AutopilotView, ChipGlass), today| {
        let (whole, short) = autopilot_title(built.0, &built.1, "", today, TitleWords::default());
        TitleReading {
            session: S,
            generation: 0,
            trail: String::new(),
            today,
            built,
            whole,
            short,
            laid: 41,
        }
    };
    // CONTROL: an ordinary generation change reads the supplied day and
    // the actual title formatter names the historical stamp's weekday.
    let mut ordinary = make(empty, None);
    let reads = Cell::new(0);
    assert!(ordinary.refresh(
        1,
        landed,
        || (None, None),
        |stamped| {
            assert!(stamped);
            reads.set(reads.get() + 1);
            (String::new(), Some(TODAY))
        }
    ));
    assert_eq!(reads.get(), 1);
    assert!(ordinary.short.ends_with("landed Fri 17:42"));
    let kept = (
        ordinary.whole.clone(),
        ordinary.short.clone(),
        ordinary.laid,
    );
    for _ in 0..3 {
        assert!(!ordinary.refresh(
            1,
            landed,
            || panic!("a kept title read its words"),
            |_| panic!("a kept title read a clock")
        ));
        assert_eq!(
            (&ordinary.whole, &ordinary.short, ordinary.laid),
            (&kept.0, &kept.1, kept.2),
        );
    }
    // CONTROL: a trail without a face clock is still read at its event.
    let mut trail = make(empty, None);
    assert!(trail.refresh(
        1,
        empty,
        || (None, None),
        |stamped| {
            assert!(!stamped);
            ("approved 14:05".to_owned(), Some(TODAY))
        }
    ));
    assert!(trail.whole.contains("approved 14:05"));
    assert!(!trail.refresh(
        1,
        empty,
        || panic!("a kept title read its words"),
        |_| panic!("an unchanged trail was read again")
    ));

    // The generation may stand while any part of the built face changes.
    // A first/restated stamp must not inherit None, or a previous day's
    // read. Clearing and restating are separate calls through the real seam.
    let moved_clock = built(
        AutopilotView::Land,
        Some(TaskFace::Landed),
        Some(stamp(18 * 60 + 5)),
    );
    let moved_view = built(AutopilotView::Cruise, landed.1.task, landed.1.task_at);
    let mut outcomes = Vec::new();
    for (from, to, day) in [
        (empty, landed, None),
        (landed, theirs, Some(FRI)),
        (landed, moved_clock, Some(FRI)),
        (landed, moved_view, Some(FRI)),
    ] {
        let mut reading = make(from, day);
        let reads = Cell::new(0);
        let rebuilt = reading.refresh(
            0,
            to,
            || (None, None),
            |stamped| {
                assert!(stamped);
                reads.set(reads.get() + 1);
                (String::new(), Some(TODAY))
            },
        );
        let expected = autopilot_title(to.0, &to.1, "", Some(TODAY), TitleWords::default());
        outcomes.push(
            rebuilt
                && reads.get() == 1
                && reading.today == Some(TODAY)
                && (reading.whole, reading.short) == expected,
        );
    }
    let mut restated = make(landed, Some(FRI));
    let clear_reads = Cell::new(0);
    let cleared = restated.refresh(
        0,
        empty,
        || (None, None),
        |stamped| {
            assert!(!stamped);
            clear_reads.set(clear_reads.get() + 1);
            (String::new(), None)
        },
    );
    outcomes.push(cleared && clear_reads.get() == 1 && restated.today.is_none());
    let restore_reads = Cell::new(0);
    let restored = restated.refresh(
        0,
        landed,
        || (None, None),
        |stamped| {
            assert!(stamped);
            restore_reads.set(restore_reads.get() + 1);
            (String::new(), Some(TODAY))
        },
    );
    outcomes.push(
        restored
            && restore_reads.get() == 1
            && restated.today == Some(TODAY)
            && restated.short.ends_with("landed Fri 17:42"),
    );
    assert_eq!(
        outcomes,
        vec![true; 6],
        "TITLE_DAY_REFRESH_REQUIRES_EACH_REBUILT_STATE"
    );
}

/// One of two windows run in lockstep: one keeps its blocks and its reads,
/// the other fits every block and reads every row anew.
struct Twin {
    app: App,
    wid: WindowId,
    session: u64,
    anew: bool,
    /// The chip blocks its frames fitted.
    fits: usize,
    _keep: (Hosted, UnixStream),
}

impl Twin {
    /// A hosted Claude Code pane in bypass, 24 × 120, the owner's facts.
    fn new(anew: bool) -> Self {
        let (mut app, wid, session, reader) = app();
        let hosted = host(&mut app);
        assert!(app.apply_term_resize(wid, 24, 120));
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
        Self {
            app,
            wid,
            session,
            anew,
            fits: 0,
            _keep: (hosted, reader),
        }
    }

    /// One frame through the capture route's splices; the fits it took.
    fn frame(&mut self) -> usize {
        let _ = crate::work_counts::take_footer_block_fits();
        let (app, wid) = (&mut self.app, self.wid);
        if self.anew {
            fit_anew_in(|| crate::claude_footer::read_every_row_in(|| frame(app, wid)));
        } else {
            frame(app, wid);
        }
        let fits = crate::work_counts::take_footer_block_fits();
        self.fits += fits;
        fits
    }

    /// The pane's rows as the glass shows them (the tab strip aside) and
    /// where each chip was painted for the pointer.
    fn glass(&self) -> (Vec<Vec<RenderCell>>, Vec<LightHit>) {
        let ws = &self.app.windows[&self.wid];
        let strip = ws.input_scratch.cells.len() - usize::from(ws.rows);
        (
            ws.input_scratch.cells[strip..].to_vec(),
            ws.claude_lights.hits.clone(),
        )
    }
}

/// What a step of the script does to a twin.
type Does = Box<dyn Fn(&mut App, WindowId, u64)>;

/// A step of the script, done to both twins alike.
type Step = (&'static str, Does);

fn step(f: impl Fn(&mut App, WindowId, u64) + 'static) -> Does {
    Box::new(f)
}

/// Claude answers: its mode row reads `mode`, the cursor back at the caret.
fn mode_row(app: &App, session: u64, mode: &str) {
    super::gesture_tests::feed(
        app,
        session,
        format!("\x1b[24;1H\x1b[2K{mode}\x1b[22;3H").as_bytes(),
    );
}

/// A key echoed at the composer's caret, column `col`.
fn typed(app: &App, session: u64, col: usize, ch: char) {
    super::gesture_tests::feed(app, session, format!("\x1b[22;{col}H{ch}").as_bytes());
}

/// The script: typing, Claude's mode deviating and back, the pointer and the
/// keyboard on each chip, a resize and back, an act lighting and dwelling
/// out, the Autopilot word cycled, a landing, a timer armed and read, and a
/// theme change.
fn script() -> Vec<Step> {
    const PLAN: &str = "  \u{23F8} plan mode on (shift+tab to cycle)";
    let t0 = Instant::now();
    let arrival = crate::harness_glass::Arrival {
        now: t0,
        unix: 1_792_159_500,
        offset_s: Some(0),
    };
    let wall_ms = 1_792_159_500_000_i64;
    let hover = |chip: Option<Chip>| {
        step(move |app, wid, session| {
            app.hover_claude_chip(wid, chip.map(|c| (session, c)));
        })
    };
    let select = |chip: Option<Chip>| {
        step(move |app, wid, session| {
            app.windows
                .get_mut(&wid)
                .expect("window")
                .claude_lights
                .selected = chip.map(|c| (session, Target::Chip(c)));
        })
    };
    let word = |word: Autopilot| {
        step(move |app, _, session| {
            assert!(app.set_session_autopilot(
                session,
                Some(word),
                crate::autopilot::Surface::Chip
            ));
        })
    };
    let resize = |cols: u16| {
        step(move |app, wid, session| {
            assert!(app.apply_term_resize(wid, 24, cols));
            draw_claude(app, session, BYPASS);
        })
    };
    vec![
        ("at rest", step(|_, _, _| {})),
        ("typed h", step(|app, _, s| typed(app, s, 3, 'h'))),
        ("typed i", step(|app, _, s| typed(app, s, 4, 'i'))),
        ("Claude in plan", step(|app, _, s| mode_row(app, s, PLAN))),
        (
            "the pointer on the light",
            hover(Some(Chip::Light(Light::Mode))),
        ),
        ("the pointer on Autopilot", hover(Some(Chip::Autopilot))),
        (
            "typed under the hover",
            step(|app, _, s| typed(app, s, 5, '!')),
        ),
        ("the pointer gone", hover(None)),
        (
            "Claude in bypass",
            step(|app, _, s| mode_row(app, s, BYPASS)),
        ),
        (
            "the keyboard on the light",
            select(Some(Chip::Light(Light::Mode))),
        ),
        ("the keyboard on Autopilot", select(Some(Chip::Autopilot))),
        ("the keyboard gone", select(None)),
        ("a resize", resize(90)),
        ("a narrow resize", resize(40)),
        (
            "the pointer on Autopilot, narrow",
            hover(Some(Chip::Autopilot)),
        ),
        ("the pointer gone, narrow", hover(None)),
        ("the resize back", resize(120)),
        (
            "an act lit",
            step(move |app, _, s| {
                crate::harness_glass::on_act_at(app, s, Act::Approved, arrival);
            }),
        ),
        (
            "typed under the label",
            step(|app, _, s| typed(app, s, 6, '?')),
        ),
        (
            "the act's dwell out",
            step(|app, _, _| {
                let end = app.harness_acts_deadline().expect("the label's end");
                app.expire_harness_acts_at(end);
            }),
        ),
        ("Autopilot: land", word(Autopilot::Land)),
        (
            "landed",
            step(move |app, _, s| {
                assert!(crate::harness_glass::on_task(
                    app,
                    s,
                    Some(TaskFace::Landed),
                    wall_ms
                ));
            }),
        ),
        (
            "the pointer on the landed chip",
            hover(Some(Chip::Autopilot)),
        ),
        ("the pointer gone again", hover(None)),
        ("Autopilot: off", word(Autopilot::Off)),
        (
            "a timer armed",
            step(move |app, _, s| {
                assert!(app.set_session_autopilot_timed(
                    s,
                    Some(Autopilot::Cruise),
                    Some(wall_ms + 2 * 3_600_000),
                    crate::autopilot::Surface::Chip
                ));
            }),
        ),
        (
            "the pointer on the timed chip",
            hover(Some(Chip::Autopilot)),
        ),
        ("the pointer off the timed chip", hover(None)),
        (
            "a theme change",
            step(|app, _, _| {
                app.apply_theme_live(aterm_render::Theme::from(
                    &aterm_types::scheme::builtin("Dracula").expect("a builtin scheme"),
                ));
            }),
        ),
        (
            "typed in the new theme",
            step(|app, _, s| typed(app, s, 7, 'x')),
        ),
    ]
}

/// THE KEPT BLOCKS AND READS DRAW WHAT FITTING AND READING ANEW DRAWS: two
/// hosted Claude Code windows run in lockstep through the script — one
/// keeping its chip blocks (`WindowLights::lend_block`), its rows' reads
/// (the engine's revisions) and its rule's run, the other fitting every
/// block and reading every row anew — show the same pane, cell for cell,
/// and the same chips to the pointer, on every step's frame and on the
/// unchanged frame after it, where the keeping window fits nothing.
///
/// NEGATIVE CONTROLS: the keeping window fitted fewer blocks than the other
/// (it kept some) and the other fitted one every frame; and a hover done to
/// one twin alone is seen.
#[test]
fn the_kept_blocks_and_reads_draw_what_fitting_and_reading_anew_draws() {
    let (mut kept, mut anew) = (Twin::new(false), Twin::new(true));
    assert_eq!(kept.session, anew.session, "the twins' sessions");
    let mut frames = 0;
    for (what, step) in script() {
        for twin in [&mut kept, &mut anew] {
            step(&mut twin.app, twin.wid, twin.session);
        }
        for pass in ["the step's frame", "the frame after"] {
            let fits = kept.frame();
            let _ = anew.frame();
            frames += 1;
            let (a, b) = (kept.glass(), anew.glass());
            if a.0 != b.0 {
                let row = (0..a.0.len()).find(|&r| a.0[r] != b.0[r]).expect("a row");
                let text = |cells: &[RenderCell]| cells.iter().map(|c| c.ch).collect::<String>();
                panic!(
                    "{what}, {pass}: row {row} differs\n kept: {:?}\n anew: {:?}",
                    text(&a.0[row]),
                    text(&b.0[row])
                );
            }
            assert_eq!(a.1, b.1, "{what}, {pass}: the chips' hits");
            if pass == "the frame after" {
                assert_eq!(fits, 0, "{what}: an unchanged frame fits nothing");
            }
        }
    }
    assert_eq!(anew.fits, frames, "CONTROL: fitting anew fits every frame");
    assert!(
        kept.fits < anew.fits / 2,
        "CONTROL: the keeping window kept ({} fits against {})",
        kept.fits,
        anew.fits
    );
    // CONTROL: the comparison sees a difference.
    kept.app
        .hover_claude_chip(kept.wid, Some((kept.session, Chip::Autopilot)));
    let _ = (kept.frame(), anew.frame());
    assert_ne!(
        kept.glass(),
        anew.glass(),
        "CONTROL: one twin's hover is seen"
    );
}
