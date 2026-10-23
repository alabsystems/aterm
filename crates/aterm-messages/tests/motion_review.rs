// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Motion deadline and ETA tests driven through the public API at injected instants.

use aterm_messages::progress::eta_words;
use aterm_messages::text::char_width;
use aterm_messages::{
    ANIM_FRAME, Amount, Duration, ELAPSED_AFTER, Eta, Hold, Instant, Links, Look, Message,
    MessageCenter, MessageId, MessageLog, Meter, Outcome, Pace, Presentation, ProgressTrack,
    Restatement, STALE_TAILED, STALE_UPDATE, STALL_AFTER, Severity, Tone, Unit, WallStamp, tags,
};

/// The instant a test mints and feeds in: the engine itself reads no clock.
#[expect(
    clippy::disallowed_methods,
    reason = "test code: the one instant a test mints and feeds in"
)]
fn minted_now() -> Instant {
    Instant::now()
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn stamp() -> WallStamp {
    WallStamp { unix_ms: 1 }
}

const TOTAL: u64 = 74_000_000;

fn meter(done: u64) -> Meter {
    Meter {
        fill_permille: Some(u16::try_from(done * 1000 / TOTAL).unwrap()),
        stats: String::new(),
        amount: Some(Amount {
            series: Amount::series_of("aterm 0.92.0"),
            done,
            total: TOTAL,
            unit: Unit::Bytes,
        }),
        load: None,
        busy: false,
    }
}

/// A live update download: determinate, with an amount (an ETA slot where
/// the width affords one).
fn download(done: u64) -> Message {
    Message::new(tags::UPDATE, Severity::Info, "Downloading aterm v0.92.0")
        .meter(meter(done))
        .hold(Hold::Live {
            stale_after: STALE_UPDATE,
        })
        .key("update.progress")
}

/// A live row with no fraction: the comet.
fn busy(title: &str) -> Message {
    Message::new(tags::PACKAGES, Severity::Info, title)
        .in_flight()
        .hold(Hold::Live {
            stale_after: STALE_TAILED,
        })
}

fn present(c: &MessageCenter, cols: usize) -> Presentation {
    c.presentation(cols, &char_width, None, Links::Painted)
}

/// What a frame DRAWS: the row's surface at its cells (the engine's
/// cell-resolution reading, [`aterm_messages::Surface::cells`] — the
/// fractions the host maps onto its window's pixels),
/// its fill's EDGE to a sixteenth of a cell (the host rasters it to the
/// pixel), the two time slots, the fade and the glyph —
/// `RowMotion` without its `anim` descriptor.
type Drawn = Vec<(
    Vec<Tone>,
    Option<u64>,
    Option<String>,
    Option<String>,
    u8,
    Option<char>,
)>;

fn drawn(c: &MessageCenter, p: &Presentation, t: Instant, look: Look) -> Drawn {
    let sixteenths = |e: u32| u64::from(e) * p.cols as u64 * 16 / u64::from(aterm_messages::ROW);
    c.motion(p, t, look)
        .rows
        .into_iter()
        .map(|r| {
            (
                r.surface.cells(p.cols),
                r.surface.edge.map(sixteenths),
                r.readout,
                r.eta,
                r.fade,
                r.glyph,
            )
        })
        .collect()
}

#[derive(Debug, Default)]
struct Audit {
    wakes: Vec<Instant>,
    wasted: usize,
    missed: Vec<String>,
}

/// Walk the host's loop from `from` to `to` (see the module doc).
fn audit(c: &MessageCenter, p: &Presentation, look: Look, from: Instant, to: Instant) -> Audit {
    let mut a = Audit::default();
    let mut t = from;
    while t < to {
        let shown = drawn(c, p, t, look);
        let d = c.motion_deadline(p, t, look);
        let stop = d.map_or(to, |d| d.min(to));
        let mut g = c.frame_instant(t) + ANIM_FRAME;
        while g < stop {
            if drawn(c, p, g, look) != shown {
                a.missed.push(format!(
                    "{look:?}: drawn at +{:?} changes at +{:?}, deadline {:?}",
                    t.duration_since(from),
                    g.duration_since(from),
                    d.map(|d| d.duration_since(from))
                ));
                break;
            }
            g += ANIM_FRAME;
        }
        let Some(d) = d else { break };
        if d >= to {
            break;
        }
        a.wakes.push(d);
        if drawn(c, p, d, look) == shown {
            a.wasted += 1;
        }
        t = d;
    }
    a
}

const FLAT: Look = Look {
    pace: Pace::Moving,
    graded: false,
};
const FLAT_STILL: Look = Look {
    pace: Pace::Still,
    graded: false,
};
const LOOKS: [Look; 4] = [Look::MOVING, Look::STILL, FLAT, FLAT_STILL];

/// A download fed every 500 ms for 8 s (its ETA latches), then silent: the
/// bytes stop at the returned instant and the stall is due `STALL_AFTER`
/// later.
fn fed(start: u64) -> (MessageCenter, MessageId, Instant) {
    let now = minted_now();
    let mut c = MessageCenter::new(MessageLog::empty(), now);
    let id = c.post(download(start), stamp(), now).id;
    c.commit_rows(now, 3);
    let (mut done, mut t) = (start, now);
    for _ in 0..16 {
        t += ms(500);
        done += 400_000;
        c.restate(
            id,
            Restatement {
                meter: Some(Some(meter(done))),
                ..Restatement::default()
            },
            t,
        );
    }
    (c, id, t)
}

/// The comet at `cols` in `look` for 75 s, its elapsed words included: the
/// frames it missed, after its cadence is held to the look's pace.
fn comet_walk(cols: usize, look: Look) -> Vec<String> {
    let now = minted_now();
    let mut c = MessageCenter::new(MessageLog::empty(), now);
    c.post(busy("Installing Homebrew"), stamp(), now);
    c.commit_rows(now, 3);
    let p = present(&c, cols);
    let a = audit(&c, &p, look, now, now + ms(75_000));
    for w in a.wakes.windows(2) {
        let gap = w[1] - w[0];
        match look.pace {
            Pace::Moving => assert!(gap >= ANIM_FRAME, "comet {cols} {look:?}: {gap:?}"),
            Pace::Still => assert!(gap >= ms(990), "comet {cols} {look:?}: {gap:?}"),
        }
    }
    if look.pace == Pace::Moving {
        assert_eq!(a.wasted, 0, "comet {cols} {look:?}: a wasted comet frame");
    }
    a.missed
        .iter()
        .map(|m| format!("comet {cols}: {m}"))
        .collect()
}

/// No missed frame, in any look: the comet at three widths for 75 s (its
/// elapsed words included), a download through its latched ETA, its glint
/// and its stall where the width lays out the ETA slot, a data glide, and
/// every echo kind for both fills — each walked frame by frame between its
/// deadlines. A moving comet wakes on the grid exactly at the frames it
/// draws something new — none missed, none wasted (a flat comet's hand-over
/// sleeps through its blank frames, and the frames where a crossing's faint
/// last sliver and the hand-over round to the same cells are skipped, not
/// drawn twice); a still look wakes at most once a second, and a
/// still echo asks no frame at all.
#[test]
fn deadlines_never_miss_a_change_the_motion_draws() {
    let comets: Vec<(usize, Look)> = [60usize, 80, 120]
        .into_iter()
        .flat_map(|cols| LOOKS.map(|look| (cols, look)))
        .collect();
    let mut missed: Vec<String> =
        aterm_spec::check::each(&comets, |&(cols, look)| comet_walk(cols, look))
            .into_iter()
            .flatten()
            .collect();
    for start in [1_000_000u64, 40_000_000] {
        for cols in [80usize, 120] {
            for look in LOOKS {
                let (c, _, last) = fed(start);
                let p = present(&c, cols);
                assert!(p.rows[0].eta.is_some(), "{cols}: the slot is laid out");
                let a = audit(&c, &p, look, last, last + ms(40_000));
                missed.extend(
                    a.missed
                        .iter()
                        .map(|m| format!("download {start}/{cols}: {m}")),
                );
            }
        }
    }
    for look in LOOKS {
        let (mut c, id, last) = fed(1_000_000);
        let t = last + ms(100);
        c.restate(
            id,
            Restatement {
                meter: Some(Some(meter(60_000_000))),
                ..Restatement::default()
            },
            t,
        );
        let p = present(&c, 120);
        let a = audit(&c, &p, look, t, t + ms(9000));
        missed.extend(a.missed.iter().map(|m| format!("glide: {m}")));
    }
    for end in ["complete", "fault", "vanish"] {
        for indeterminate in [false, true] {
            for look in LOOKS {
                let now = minted_now();
                let mut c = MessageCenter::new(MessageLog::empty(), now);
                let msg = if indeterminate {
                    busy("Installing Homebrew")
                } else {
                    download(30_000_000)
                };
                let id = c.post(msg, stamp(), now).id;
                c.commit_rows(now, 3);
                let at = now + ms(5000);
                match end {
                    "complete" => assert!(c.resolve(id, Outcome::Ok, at)),
                    "fault" => assert!(c.resolve(id, Outcome::Warn, at)),
                    _ => assert!(c.withdraw(id, at)),
                }
                let p = present(&c, 120);
                let until = c.echoes().first().expect("an echo").until;
                let a = audit(&c, &p, look, at, until);
                missed.extend(a.missed.iter().map(|m| format!("echo {end}: {m}")));
                if look.pace == Pace::Still {
                    assert!(a.wakes.is_empty(), "echo {end}: a still echo asked a frame");
                }
            }
        }
    }
    assert!(missed.is_empty(), "missed frames:\n{}", missed.join("\n"));
}

/// A bar wakes only for a frame that draws something new (the glint's
/// centre off a short fill at either end of its travel, or glides inside
/// one eighth of a cell): a download at short and long fills
/// and two widths, and a data glide of half a per-mille, waste no wake in the
/// moving look — and still miss none.
#[test]
fn a_bar_never_wakes_for_a_frame_that_draws_nothing_new() {
    for start in [1_000_000u64, 7_400_000, 40_000_000] {
        for cols in [60usize, 80, 120] {
            for look in [Look::MOVING, FLAT] {
                let (c, _, last) = fed(start);
                let p = present(&c, cols);
                let a = audit(&c, &p, look, last, last + ms(20_000));
                assert!(a.missed.is_empty(), "{start}/{cols}: {:?}", a.missed);
                assert_eq!(a.wasted, 0, "{start}/{cols} {look:?}: a wasted bar wake");
            }
        }
    }
    let (mut c, id, last) = fed(1_000_000);
    let t = last + ms(100);
    c.restate(
        id,
        Restatement {
            meter: Some(Some(meter(8_437_000))),
            ..Restatement::default()
        },
        t,
    );
    for cols in [60usize, 120] {
        let p = present(&c, cols);
        let a = audit(&c, &p, Look::MOVING, t, t + ms(1000));
        assert!(a.missed.is_empty(), "glide {cols}: {:?}", a.missed);
        assert_eq!(a.wasted, 0, "glide {cols}: a wasted glide wake");
    }
}

/// Where the width starves the ETA slot (52 columns here, over the
/// full-width meter), the stall's tone change (`Tone::STALLED`) is the only
/// stall signal left on the row, so `motion_deadline` must schedule a wake
/// for it in both `Look::STILL` and `Look::MOVING`, regardless of bar length.
#[test]
fn a_stall_repaints_the_bar_even_where_the_eta_slot_starved() {
    let mut missed = Vec::new();
    for (start, look) in [
        (40_000_000u64, Look::STILL),
        (1_000_000, Look::STILL),
        (1_000_000, Look::MOVING),
    ] {
        let (c, _, last) = fed(start);
        let p = present(&c, 52);
        assert!(p.rows[0].eta.is_none(), "52 columns starve the ETA slot");
        let stalled_at = last + STALL_AFTER + ANIM_FRAME;
        assert!(
            drawn(&c, &p, stalled_at, look)[0]
                .0
                .contains(&Tone::STALLED),
            "{start} {look:?}: the motion draws the stall"
        );
        let a = audit(&c, &p, look, last, last + STALL_AFTER * 2);
        missed.extend(a.missed.iter().map(|m| format!("{start}: {m}")));
    }
    assert!(missed.is_empty(), "missed:\n{}", missed.join("\n"));
}

/// A deterministic stream of readings from a small LCG: gaps of 50 ms–4 s,
/// advances of 0–3 MB, the odd regression and change of total.
fn stream(seed: u64, n: usize) -> Vec<(u64, Amount)> {
    let mut x = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    let mut next = move || {
        x = x
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        x >> 33
    };
    let unit = if seed.is_multiple_of(2) {
        Unit::Bytes
    } else {
        Unit::Steps
    };
    let (mut at, mut done, mut total) = (0u64, 0u64, 50_000_000u64);
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        at += 50 + next() % 3950;
        match next() % 40 {
            0 => done = done.saturating_sub(next() % 2_000_000),
            1 => total += next() % 10_000_000,
            _ => done = (done + next() % 3_000_000).min(total),
        }
        out.push((
            at,
            Amount {
                series: 9,
                done,
                total,
                unit,
            },
        ));
    }
    out
}

/// Over 64 random streams (bytes and steps; bursts, droughts, regressions, a
/// growing total), an ETA number appears only after at least five readings
/// spanning at least five seconds since the estimator last started over
/// (three samples for the first secant over `RATE_MIN_SPAN`, then three
/// agreeing projections over `STABLE_MIN`); it is at most 48 h; its words are
/// `<5 s left` or `N … left` — no `~`, since `left` already says it is an
/// estimate — never zero; and between two readings it never grows.
#[test]
fn the_eta_never_speaks_from_too_little_and_never_goes_negative_or_absurd() {
    for seed in 0..64u64 {
        let base = minted_now();
        let mut t = ProgressTrack::default();
        let readings = stream(seed, 200);
        let (mut since, mut seen, mut prev_done) = (0u64, 0usize, None::<u64>);
        for (i, (at, a)) in readings.iter().enumerate() {
            let now = base + ms(*at);
            if prev_done.is_none_or(|p| a.done < p) {
                since = *at;
                seen = 0;
            }
            prev_done = Some(a.done);
            t.observe(now, *a);
            seen += 1;
            let until = readings.get(i + 1).map_or(*at + 60_000, |(n, _)| *n);
            let mut last_left: Option<Duration> = None;
            let mut probe = *at;
            while probe < until {
                if let Eta::Remaining(left) = t.eta(base + ms(probe)) {
                    assert!(
                        seen >= 5 && probe - since >= 5000,
                        "seed {seed}: an ETA after {seen} readings over {} ms",
                        probe - since
                    );
                    assert!(left <= Duration::from_hours(48), "seed {seed}: {left:?}");
                    if let Some(w) = eta_words(left) {
                        assert!(
                            w == "<5 s left" || (w.ends_with(" left") && !w.contains('~')),
                            "seed {seed}: {w:?}"
                        );
                        assert!(!w.starts_with("0 "), "seed {seed}: {w:?}");
                    }
                    if let Some(prev) = last_left {
                        assert!(left <= prev, "seed {seed}: the ETA grew between readings");
                    }
                    last_left = Some(left);
                }
                probe += 250;
            }
        }
    }
}

/// Once the latched completion instant is overdue by more than its grace
/// window with no further advance, `eta()` unlatches (`Hidden`) for work
/// counted in `Unit::Steps` or `Unit::Items` rather than continuing to
/// report `<5 s left` indefinitely.
#[test]
fn an_overdue_eta_stops_promising_under_five_seconds() {
    for unit in [Unit::Steps, Unit::Items] {
        let base = minted_now();
        let mut t = ProgressTrack::default();
        let mut now = base;
        for k in 0..20u64 {
            t.observe(
                now,
                Amount {
                    series: 7,
                    done: k * 10,
                    total: 1000,
                    unit,
                },
            );
            now += ms(500);
        }
        let Eta::Remaining(left) = t.eta(now) else {
            panic!("{unit:?}: a steady rate latches");
        };
        let overdue = now + left + Duration::from_secs(60);
        let said = match t.eta(overdue) {
            Eta::Remaining(r) => eta_words(r),
            _ => None,
        };
        assert_eq!(
            said, None,
            "{unit:?}: a minute past its own estimate, with nothing new, it still says {said:?}"
        );
    }
}

/// The elapsed clock and its deadline agree to the frame, from the first
/// word at `ELAPSED_AFTER`, in the still look — an unfocused window's only
/// wake: no missed tick, no wasted one, the first exactly on the grid step
/// at `ELAPSED_AFTER`.
#[test]
fn the_elapsed_clock_ticks_exactly_on_its_deadlines() {
    let now = minted_now();
    let mut c = MessageCenter::new(MessageLog::empty(), now);
    c.post(busy("Installing Homebrew"), stamp(), now);
    c.commit_rows(now, 3);
    let p = present(&c, 120);
    let a = audit(&c, &p, Look::STILL, now, now + ms(130_000));
    assert!(a.missed.is_empty(), "{}", a.missed.join("\n"));
    assert_eq!(a.wasted, 0);
    let first = a.wakes.first().copied().expect("the first word");
    assert!(first >= now + ELAPSED_AFTER && first < now + ELAPSED_AFTER + ANIM_FRAME);
}

/// A same-activity supersede by key preserves the activity's started-at
/// instant so the elapsed readout continues counting from the original post
/// rather than resetting.
#[test]
fn the_elapsed_clock_survives_a_same_activity_supersede() {
    let now = minted_now();
    let mut c = MessageCenter::new(MessageLog::empty(), now);
    c.post(
        busy("Installing ALab toolchain")
            .line("installing 2 ALab program(s)")
            .key("toolchain.pass"),
        stamp(),
        now,
    );
    c.commit_rows(now, 3);
    let p = present(&c, 120);
    let before = c.motion(&p, now + ms(30_000), Look::MOVING).rows[0]
        .readout
        .clone()
        .expect("the clock runs at 30 s");
    let second = c
        .post(
            busy("Installing ALab toolchain")
                .line("installing 3 ALab program(s)")
                .key("toolchain.pass"),
            stamp(),
            now + ms(30_000),
        )
        .id;
    c.commit_rows(now + ms(30_000), 3);
    let p = present(&c, 120);
    assert_eq!(
        c.live(second).unwrap().motion_since,
        Some(now),
        "the comet's epoch runs on"
    );
    let after = c.motion(&p, now + ms(35_000), Look::MOVING).rows[0]
        .readout
        .clone();
    assert_eq!(
        after.as_deref(),
        Some("for 34 s"),
        "the clock read {before:?} at 30 s"
    );
}

/// A fault echo's hold asks no frame: a bar's
/// warn wash stands still from `ECHO_FAULT_CROSS` to `ECHO_FAULT_FLASH`, a
/// busy row's drain from `ECHO_DRAIN` — no display frame there draws
/// anything new, so none wakes; the fade's first frame is the next wake, and
/// nothing is missed.
#[test]
fn a_fault_echo_never_wakes_through_its_hold() {
    for indeterminate in [false, true] {
        for cols in [60usize, 80, 120] {
            let now = minted_now();
            let mut c = MessageCenter::new(MessageLog::empty(), now);
            let msg = if indeterminate {
                busy("Installing Homebrew")
            } else {
                download(30_000_000)
            };
            let id = c.post(msg, stamp(), now).id;
            c.commit_rows(now, 3);
            let at = now + ms(5000);
            assert!(c.resolve(id, Outcome::Warn, at));
            let p = present(&c, cols);
            let (started, until) = {
                let e = c.echoes().first().expect("an echo");
                (e.started, e.until)
            };
            let a = audit(&c, &p, Look::MOVING, at, until);
            assert!(
                a.missed.is_empty(),
                "{indeterminate}/{cols}: {:?}",
                a.missed
            );
            let settled = if indeterminate {
                aterm_messages::ECHO_DRAIN
            } else {
                aterm_messages::ECHO_FAULT_CROSS
            };
            let in_hold: Vec<_> = a
                .wakes
                .iter()
                .map(|w| w.duration_since(started))
                .filter(|t| *t > settled + c.refresh() && *t <= aterm_messages::ECHO_FAULT_FLASH)
                .collect();
            assert!(
                in_hold.is_empty(),
                "{indeterminate}/{cols}: woke in the hold at {in_hold:?}"
            );
            assert!(
                a.wasted <= 1,
                "{indeterminate}/{cols}: {} wakes drew nothing of {}",
                a.wasted,
                a.wakes.len()
            );
        }
    }
}
