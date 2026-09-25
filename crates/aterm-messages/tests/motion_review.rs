// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The reviewer's proofs of the motion layer's DEADLINES and the ETA's
//! honesty (design §10.4.4, §10.4.6, §10.8; review 2026-09-24), driven only
//! through the crate's public API at injected instants.
//!
//! The central tool is [`audit`]: it walks the host's loop — wake at each
//! `motion_deadline`, prepare the frame there — and between two wakes looks
//! at EVERY frame on the 33 ms grid for a change the deadline did not ask for.
//! Such a change is a MISSED frame: the pixels on glass are stale until some
//! unrelated wake repaints the window, and on an idle, unfocused window that
//! may be never. A wake at which nothing drawable changed is a WASTED wake.
//!
//! The tests headed `DEFECT (review 2026-09-24)` pinned a defect the review
//! found; each failed then and passes with its fix (the final fixer's round,
//! design §10.14 rulings 108-110).

use aterm_messages::progress::eta_words;
use aterm_messages::text::char_width;
use aterm_messages::{
    ANIM_FRAME, Amount, Duration, ELAPSED_AFTER, Eta, Hold, Instant, Links, Look, Message,
    MessageCenter, MessageId, MessageLog, Meter, Outcome, Pace, Presentation, ProgressTrack,
    Restatement, STALE_TAILED, STALE_UPDATE, STALL_AFTER, Severity, Tone, Unit, WallStamp, tags,
};

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
        level: false,
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
/// fractions the host maps onto its window's pixels, design ruling 140),
/// the two time slots, the fade and the glyph — `RowMotion` without its
/// `anim` descriptor.
type Drawn = Vec<(Vec<Tone>, Option<String>, Option<String>, u8, Option<char>)>;

fn drawn(c: &MessageCenter, p: &Presentation, t: Instant, look: Look) -> Drawn {
    c.motion(p, t, look)
        .rows
        .into_iter()
        .map(|r| (r.surface.cells(p.cols), r.readout, r.eta, r.fade, r.glyph))
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
    let now = Instant::now();
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

/// NEGATIVE CONTROL for host deadline memoization: the painted percent and
/// current ETA word can stay the same while a new reading moves the *next*
/// ETA boundary. A cache keyed only by today's visible text would miss it.
#[test]
fn same_painted_progress_can_move_the_next_eta_boundary() {
    let mut witness = None;
    for start in [1_000_000, 10_000_000, 25_000_000, 40_000_000, 55_000_000] {
        let (base, id, last) = fed(start);
        let mut before_layout = present(&base, 120);
        assert!(before_layout.rows[0].eta.is_some(), "ETA slot is painted");
        // Compare the ETA clock itself, without a simultaneous elapsed-word
        // tick masking its deadline in a layout that has both slots.
        before_layout.rows[0].elapsed = None;
        let old_meter = base.live(id).unwrap().msg.meter.as_ref().unwrap().clone();
        for delay in [500, 1000, 2000, 3000, 4000, 5000, 6000, 7000, 8000, 9000] {
            let at = last + ms(delay);
            let word = base.motion(&before_layout, at, Look::STILL).rows[0]
                .eta
                .clone();
            let next = base.motion_deadline(&before_layout, at, Look::STILL);
            for advance in [0, 100, 1_000, 10_000, 50_000, 100_000, 200_000] {
                let mut candidate = base.clone();
                let mut reading = old_meter.clone();
                reading.amount.as_mut().unwrap().done += advance;
                // The UI still says the same whole percent: this is new
                // estimator evidence, not a different painted fill.
                assert!(candidate.restate(
                    id,
                    Restatement {
                        meter: Some(Some(reading)),
                        ..Restatement::default()
                    },
                    at,
                ));
                let mut after_layout = present(&candidate, 120);
                after_layout.rows[0].elapsed = None;
                let after_word = candidate.motion(&after_layout, at, Look::STILL).rows[0]
                    .eta
                    .clone();
                let after_next = candidate.motion_deadline(&after_layout, at, Look::STILL);
                if before_layout.rows[0].pct == after_layout.rows[0].pct
                    && word == after_word
                    && next.is_some()
                    && after_next.is_some()
                    && next != after_next
                {
                    witness = Some((start, delay, advance, next, after_next));
                    break;
                }
            }
            if witness.is_some() {
                break;
            }
        }
        if witness.is_some() {
            break;
        }
    }
    assert!(
        witness.is_some(),
        "an unchanged painted percent/ETA can still need a new future deadline"
    );
}

// ---- deadlines vs. what is drawn ---------------------------------------------

/// NO MISSED FRAME, IN ANY LOOK: the comet at three widths for 75 s (its
/// elapsed words included), a download through its latched ETA, its glint
/// and its stall where the width lays out the ETA slot, a data glide, and
/// every echo kind for both fills — each walked frame by frame between its
/// deadlines. And the CADENCE: a moving comet wakes on the grid, at most
/// `SPIN_FRAMES` steps apart (the spinner turns at least that often), and
/// never wastes a wake — the frames where a crossing's faint last sliver
/// and the hand-over round to the same cells are skipped, not drawn twice
/// (design ruling 140); a still look wakes at most once a second, and a
/// still echo asks no frame at all.
#[test]
fn deadlines_never_miss_a_change_the_motion_draws() {
    let mut missed = Vec::new();
    for cols in [60usize, 80, 120] {
        for look in LOOKS {
            let now = Instant::now();
            let mut c = MessageCenter::new(MessageLog::empty(), now);
            c.post(busy("Installing Homebrew"), stamp(), now);
            c.commit_rows(now, 3);
            let p = present(&c, cols);
            let a = audit(&c, &p, look, now, now + ms(75_000));
            missed.extend(a.missed.iter().map(|m| format!("comet {cols}: {m}")));
            for w in a.wakes.windows(2) {
                let gap = w[1] - w[0];
                match look.pace {
                    Pace::Moving => assert!(
                        gap >= ANIM_FRAME && gap <= ANIM_FRAME * aterm_messages::SPIN_FRAMES,
                        "comet {cols} {look:?}: {gap:?}"
                    ),
                    Pace::Still => assert!(gap >= ms(990), "comet {cols} {look:?}: {gap:?}"),
                }
            }
            if look.pace == Pace::Moving {
                assert_eq!(a.wasted, 0, "comet {cols} {look:?}: a wasted comet frame");
            }
        }
    }
    // The download at widths that lay out its ETA slot (the starved slot is
    // pinned on its own, below).
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
    // A data glide.
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
    // Every echo, for a bar and for a comet.
    for end in ["complete", "fault", "vanish"] {
        for indeterminate in [false, true] {
            for look in LOOKS {
                let now = Instant::now();
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

/// A BAR WAKES ONLY FOR A FRAME THAT DRAWS SOMETHING NEW (review
/// 2026-09-24: 38 of 131 wakes of a 10 % download at 80 columns drew the
/// same cells — the glint's centre off a short fill at either end of its
/// travel, and glides inside one eighth): a download at short and long fills
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

/// DEFECT (review 2026-09-24): A STALL NEVER REPAINTS WHERE THE ETA SLOT
/// STARVED. `motion_deadline` folds the stall's onset only for a MOVING,
/// graded bar at least 1.5 cells long (center.rs, the `look.graded && f >=
/// 384` arm), or through the ETA slot's `next_change`. Where the width
/// starves that slot (52 columns here, over the full-width meter) the
/// stall's tone change —
/// `stalled_bar`, `Tone::STALLED`, the ONLY stall signal left on such a row,
/// since the `stalled` word has no slot — is drawn by `motion` but asked by
/// no deadline in the STILL look (every unfocused window, Reduce Motion,
/// Serious Mode) nor for a short bar in the moving look. On an idle window
/// the bar keeps its live ink after the bytes stop, until something else
/// repaints it. Fix: fold `track.next_change(q)` for every graded
/// determinate live row with an amount, whatever the pace, the slot and the
/// bar's length.
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

// ---- the ETA --------------------------------------------------------------------

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

/// THE ETA NEVER SPEAKS FROM TOO LITTLE, NEVER GOES NEGATIVE OR ABSURD, AND
/// ONLY COUNTS DOWN BETWEEN READINGS: over 64 random streams (bytes and
/// steps; bursts, droughts, regressions, a growing total), a number appears
/// only after at least five readings spanning at least five seconds since
/// the estimator last started over (three samples for the first secant over
/// `RATE_MIN_SPAN`, then three agreeing projections over `STABLE_MIN`); it
/// is at most 48 h; its words are `<5 s left` or `~… left`, never zero; and between
/// two readings it never grows.
#[test]
fn the_eta_never_speaks_from_too_little_and_never_goes_negative_or_absurd() {
    for seed in 0..64u64 {
        let base = Instant::now();
        let mut t = ProgressTrack::default();
        let readings = stream(seed, 200);
        let (mut since, mut seen, mut prev_done) = (0u64, 0usize, None::<u64>);
        for (i, (at, a)) in readings.iter().enumerate() {
            let now = base + ms(*at);
            // A regression starts the estimator over (so does a new series
            // or unit, which a stream never changes).
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
                        assert!(w == "<5 s left" || w.starts_with('~'), "seed {seed}: {w:?}");
                        assert!(!w.starts_with("~0 "), "seed {seed}: {w:?}");
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

/// DEFECT (review 2026-09-24): AN OVERDUE ETA READS `<5 s` FOREVER FOR WORK
/// COUNTED IN STEPS OR ITEMS. Once the latched completion instant passes
/// with no further reading, `eta()` answers `Remaining(0)` — `<5 s` — for as
/// long as the row lives: a minute (or ten) later it still promises "under
/// five seconds". Bytes are rescued by the stall after `STALL_AFTER`; steps
/// and items (the toolchain row's `Unit::Steps` amount) have no exit. The
/// host's toolchain lane masks it today only because atpkg's 2 s heartbeat
/// re-feeds the same `done`, which unlatches the ETA; a reporter that
/// restates only on change shows the lie. Fix: once the anchor is overdue by
/// more than a grace (e.g. `max(5 s, a fifth of the estimate)`) with no
/// advance, unlatch (`Hidden`), and give `next_change` that instant so the
/// slot clears on time.
#[test]
fn an_overdue_eta_stops_promising_under_five_seconds() {
    for unit in [Unit::Steps, Unit::Items] {
        let base = Instant::now();
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

// ---- the elapsed clock -------------------------------------------------------

/// The elapsed clock and its deadline agree to the frame, from the first
/// word at `ELAPSED_AFTER`, in the still look — an unfocused window's only
/// wake: no missed tick, no wasted one, the first exactly on the grid step
/// at `ELAPSED_AFTER`.
#[test]
fn the_elapsed_clock_ticks_exactly_on_its_deadlines() {
    let now = Instant::now();
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

/// DEFECT (review 2026-09-24): A SAME-ACTIVITY SUPERSEDE RESTARTS THE
/// ELAPSED CLOCK. The readout is `elapsed_words(q − posted_at)`, and a
/// supersede by key is a new post — so work that re-words its row
/// (`announce_toolchain_pass` twice; any reporter that re-titles a phase of
/// the same work) shows `0:29`, then nothing for ten seconds, then `0:10`,
/// while the comet's epoch (`motion_since`, inherited on a same-activity
/// supersede) runs on — and the accessibility description, which speaks
/// `elapsed_spoken(now − motion_since)` (app_settings.rs), says "running for
/// 45 seconds" beside a band reading `0:14` (and lags the band by the reveal
/// grace on every row posted with `reveal_after`). Fix: one clock for the
/// work — carry the started-at instant across a same-activity supersede (as
/// the epoch is) and have the band and the description read the same one.
#[test]
fn the_elapsed_clock_survives_a_same_activity_supersede() {
    let now = Instant::now();
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
    // The frame grid floors 35 s to 34.98 s: `0:34` is the running clock.
    assert_eq!(
        after.as_deref(),
        Some("0:34"),
        "the clock read {before:?} at 30 s"
    );
}
