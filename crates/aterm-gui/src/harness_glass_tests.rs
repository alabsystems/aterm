// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ACT TICKER'S GLASS (`docs/DESIGN-harness-autopilot-2026-10-15.md`
//! §9.2.5): coalescing on an injected clock, the switches that withhold the
//! label and keep the trail, the trail as the title lists it, and the one
//! deadline.

use super::*;

use aterm_agent::supervise::Autopilot;

use crate::autopilot::AutopilotView;
use crate::claude_lights::autopilot_title;

/// A switch a test flips (`true`: the label withheld), by name.
type Switch = (&'static str, fn(&mut App, bool));

/// 2026-10-16 14:05:00 UTC, a Friday: the wall second the tests' acts
/// arrive around, read in UTC.
const T0_UNIX: i64 = 1_792_159_500;

/// An act's arrival `ms` after `t0` (`unix` moving with it), in UTC.
fn at(t0: Instant, ms: u64) -> Arrival {
    Arrival {
        now: t0 + Duration::from_millis(ms),
        unix: T0_UNIX + i64::try_from(ms / 1000).expect("small"),
        offset_s: Some(0),
    }
}

/// The trail's acts, newest first.
fn trail(g: &SessionGlass) -> Vec<Act> {
    g.trail.iter().flatten().map(|e| e.act).collect()
}

/// COALESCING, ON AN INJECTED CLOCK (§9.2.1): five acts in a second give
/// ONE label, showing the newest, which ends at the first + 3 s — the dwell
/// is never extended, so a stream cannot hold it lit; an act at +10 s lights
/// nothing (the quiet), and one at +15 s does; a three-key dialog in 200 ms
/// lights one label and leaves one trail entry, and so does one whose keys
/// come 1.8 s apart, as the real dialog's do.
///
/// NEGATIVE CONTROLS: the +10 s act on a glass with no label behind it
/// lights at once (the quiet is what held it); the dialog's key again after
/// the dwell is a new trail entry (the repeat rule is the dwell's alone).
#[test]
fn the_ticker_coalesces_a_burst_into_one_label_per_quiet() {
    let t0 = Instant::now();
    let mut g = SessionGlass::default();
    let burst = [
        Act::Approved,
        Act::Continued,
        Act::Answered,
        Act::Resumed,
        Act::Declined,
    ];
    let mut labels = std::collections::BTreeSet::new();
    for (i, act) in burst.into_iter().enumerate() {
        let took = g.take(act, at(t0, 200 * i as u64), true);
        assert!(took.trail, "{act:?} joins the trail");
        assert!(took.lit, "{act:?} replaces the label in place");
        labels.extend(g.lit_until());
    }
    assert_eq!(labels.len(), 1, "one label for the burst");
    assert_eq!(g.chip().lit, Some(Act::Declined), "the newest shows");
    assert_eq!(g.lit_until(), Some(t0 + DWELL), "the first's dwell");
    assert_eq!(trail(&g), [Act::Declined, Act::Resumed, Act::Answered]);
    // Still lit just before its end, gone at it.
    assert!(!g.expire(t0 + DWELL - Duration::from_millis(1)));
    assert!(g.expire(t0 + DWELL));
    assert_eq!((g.chip().lit, g.lit_until()), (None, None));
    // +10 s: inside the quiet — the trail only.
    let took = g.take(Act::Approved, at(t0, 10_000), true);
    assert_eq!(
        took,
        Took {
            lit: false,
            trail: true
        }
    );
    assert_eq!(g.chip().lit, None);
    assert_eq!(g.chip().last.map(|(act, _)| act), Some(Act::Approved));
    // +15 s: the quiet has passed.
    assert!(g.take(Act::Continued, at(t0, 15_000), true).lit);
    assert_eq!(
        g.lit_until(),
        Some(t0 + Duration::from_millis(15_000) + DWELL)
    );
    // NEGATIVE CONTROL: no label behind it, the +10 s act lights.
    let mut fresh = SessionGlass::default();
    assert!(fresh.take(Act::Approved, at(t0, 10_000), true).lit);

    // A three-key dialog in 200 ms: one label, one trail entry.
    let mut d = SessionGlass::default();
    let keys: Vec<Took> = [0, 100, 200]
        .into_iter()
        .map(|ms| d.take(Act::Answered, at(t0, ms), true))
        .collect();
    assert_eq!(
        keys,
        [
            Took {
                lit: true,
                trail: true
            },
            Took::default(),
            Took::default()
        ]
    );
    assert_eq!(trail(&d), [Act::Answered]);
    assert_eq!(d.trail_gen(), 1);
    assert_eq!(
        d.lit_until(),
        Some(t0 + Duration::from_millis(200) + DWELL),
        "a dwell past the dialog's last key"
    );
    // NEGATIVE CONTROL: the same act past the dwell is a new entry.
    assert!(d.take(Act::Answered, at(t0, 3_500), true).trail);
    assert_eq!(trail(&d), [Act::Answered, Act::Answered]);

    // A dialog as Claude Code 2.1.301 takes it (measured in the lab): its
    // three keys 1.8 s apart, 3.6 s in all, are still one entry — each key
    // within the dwell of the one before it.
    let mut real = SessionGlass::default();
    let keys: Vec<bool> = [0, 1_800, 3_600]
        .into_iter()
        .map(|ms| real.take(Act::Answered, at(t0, ms), true).trail)
        .collect();
    assert_eq!(keys, [true, false, false]);
    assert_eq!(trail(&real), [Act::Answered]);
    assert_eq!(real.trail[0].map(|e| e.at), Some(t0), "at its first key");
}

/// A RUN OF ONE ACT IS ONE ENTRY PER BURST, NEVER ONE FOREVER (§9.2.1): an
/// approval every 2.5 s for a minute — each a repeat within the dwell of
/// the one before — is a new trail entry, with its own clock time, once
/// [`BURST`] has passed since its entry's first act, and the label lights
/// again once the quiet has passed; the newest entry's clock is the run's
/// late time, not its first. A run of approvals cannot hold a label lit:
/// each label still ends a dwell after it lit.
///
/// NEGATIVE CONTROL: inside the burst the run is one entry.
#[test]
fn a_run_of_one_act_is_a_new_entry_past_the_burst() {
    let t0 = Instant::now();
    let mut g = SessionGlass::default();
    let mut labels = 0;
    for i in 0..=24 {
        let at = at(t0, 2_500 * i);
        let took = g.take(Act::Approved, at, true);
        if took.lit {
            labels += 1;
            assert_eq!(
                g.lit_until(),
                Some(at.now + DWELL),
                "lit {i}: its own dwell"
            );
        }
        g.expire(at.now);
        // NEGATIVE CONTROL: inside the burst, one entry.
        if 2_500 * i < 9_000 {
            assert_eq!(g.trail_gen(), 1, "act {i} inside the burst");
        }
    }
    // Entries at 0, 10, 20, … 60 s (each burst's first act after 9 s): 7.
    assert_eq!(g.trail_gen(), 7, "one entry per burst");
    assert_eq!(labels, 4, "lit at 0, 20, 40 and 60 s: one per quiet");
    let newest = g.trail[0].expect("an entry");
    assert_eq!(newest.at, t0 + Duration::from_secs(60));
    assert_eq!(newest.clock.as_str(), "14:06", "the run's late time");
}

/// A QUESTION DIALOG'S LABEL STAYS UP A DWELL PAST ITS LAST KEY (§9.2.7):
/// the dialog covers the composer while its keys are pressed — no chip and
/// no label is drawn there — so the label its first key lit is kept up
/// [`DWELL`] past each later key: a two-question dialog's three keys 1.8 s
/// apart (Claude Code 2.1.301) keep it lit until 3 s after the third, one
/// label and one trail entry. The quiet still runs from the first key.
///
/// NEGATIVE CONTROLS: approvals a second apart are not kept up (a stream of
/// boxes is no dialog); a dialog's key under a label of another act keeps
/// nothing; a key that may not light keeps nothing.
#[test]
fn a_dialogs_label_stays_up_a_dwell_past_its_last_key() {
    let t0 = Instant::now();
    let mut g = SessionGlass::default();
    for ms in [0, 1_800, 3_600] {
        g.take(Act::Answered, at(t0, ms), true);
    }
    let end = t0 + Duration::from_millis(3_600) + DWELL;
    assert_eq!(g.lit_until(), Some(end));
    assert!(!g.expire(end - Duration::from_millis(1)), "still up");
    assert_eq!(g.chip().lit, Some(Act::Answered));
    assert!(g.expire(end));
    assert_eq!(trail(&g), [Act::Answered], "one entry");
    // The quiet ran from the first key.
    assert!(!g.take(Act::Approved, at(t0, 14_000), true).lit);
    assert!(g.take(Act::Continued, at(t0, 15_000), true).lit);

    // NEGATIVE CONTROL: boxes a second apart.
    let mut boxes = SessionGlass::default();
    for ms in [0, 1_000, 2_000] {
        boxes.take(Act::Approved, at(t0, ms), true);
    }
    assert_eq!(boxes.lit_until(), Some(t0 + DWELL));
    // NEGATIVE CONTROL: a key under another act's label.
    let mut other = SessionGlass::default();
    other.take(Act::Approved, at(t0, 0), true);
    other.take(Act::Answered, at(t0, 500), false);
    other.take(Act::Answered, at(t0, 1_000), true);
    assert_eq!(other.chip().lit, Some(Act::Approved));
    assert_eq!(other.lit_until(), Some(t0 + DWELL));
    // NEGATIVE CONTROL: a later key that may not light.
    let mut dark = SessionGlass::default();
    dark.take(Act::Answered, at(t0, 0), true);
    dark.take(Act::Answered, at(t0, 1_800), false);
    assert_eq!(dark.lit_until(), Some(t0 + DWELL));
}

/// THE NEWEST ACT IS SPOKEN BY THE TITLE'S RULE (§9.2.1, §9.0 "Nothing goes
/// stale on the glass"): `Last: approved at 14:05.` on its own date, with
/// its whole weekday read on another (`on Friday`), and nothing at all a day
/// and a minute after it — the screen reader's description never reads an
/// old act as today's.
#[test]
fn the_newest_act_is_spoken_by_the_titles_rule() {
    let t0 = Instant::now();
    let today = T0_UNIX.div_euclid(86_400);
    let mut g = SessionGlass::default();
    assert_eq!(g.spoken_last(T0_UNIX, today), None, "no act");
    g.take(Act::Approved, at(t0, 0), true);
    assert_eq!(
        g.spoken_last(T0_UNIX, today).as_deref(),
        Some("Last: approved at 14:05.")
    );
    assert_eq!(
        g.spoken_last(T0_UNIX + 43_200, today + 1).as_deref(),
        Some(
            format!(
                "Last: approved on {} at 14:05.",
                Weekday::of_days(today).long()
            )
            .as_str()
        ),
        "read the next day"
    );
    assert_eq!(
        g.spoken_last(T0_UNIX + 86_460, today + 1),
        None,
        "a day old"
    );
}

/// AN ACT THAT MAY NOT LIGHT goes into the trail alone: no label, no
/// deadline, no quiet started — so the next act that may light does.
#[test]
fn an_act_that_may_not_light_keeps_only_its_trail_entry() {
    let t0 = Instant::now();
    let mut g = SessionGlass::default();
    assert_eq!(
        g.take(Act::Approved, at(t0, 0), false),
        Took {
            lit: false,
            trail: true
        }
    );
    assert_eq!((g.chip().lit, g.lit_until()), (None, None));
    assert!(g.take(Act::Continued, at(t0, 500), true).lit, "no quiet");
}

/// The App with a harness host, so its session 0 runs under the table's
/// Cruise; the guard shuts the host down.
struct Hosted(crate::harness_host::HostHandle);

impl Drop for Hosted {
    fn drop(&mut self) {
        self.0.shutdown_and_join();
    }
}

fn hosted() -> (App, Hosted) {
    let mut app = App::headless_for_test();
    let host = crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    );
    app.harness = Some(host.clone());
    (app, Hosted(host))
}

/// `[claude_footer] acts`.
fn acts(app: &mut App, on: bool) {
    app.config.claude_footer = Some(crate::app_config::ClaudeFooterConfig {
        acts: Some(on),
        ..crate::app_config::ClaudeFooterConfig::default()
    });
}

/// SERIOUS MODE AND `acts = false` EACH LIGHT NOTHING AND KEEP THE TRAIL
/// (§9.2.1), and so does a session the harness carries no work for (Off):
/// no label, no deadline, the newest act still the chip's `last`. Turned
/// on again mid-dwell elsewhere, a label lit before shows again — the
/// switch is read where the frame reads the glass.
///
/// CONTROL: with both on, under Cruise, the act lights and arms the
/// deadline at its end.
#[test]
fn serious_mode_and_acts_off_light_nothing_and_keep_the_trail() {
    let t0 = Instant::now();
    let (mut app, _host) = hosted();
    assert_eq!(app.session_autopilot(0).1, AutopilotView::Cruise);
    // CONTROL.
    on_act_at(&mut app, 0, Act::Approved, at(t0, 0));
    assert_eq!(app.chip_glass(0).lit, Some(Act::Approved));
    assert_eq!(app.harness_acts_deadline(), Some(t0 + DWELL));
    app.expire_harness_acts_at(t0 + DWELL);
    assert_eq!(app.harness_acts_deadline(), None);

    let withheld: [Switch; 3] = [
        ("acts = false", |app, on| acts(app, !on)),
        ("serious mode", |app, on| app.serious_mode = on),
        ("Off", |app, on| {
            let word = if on {
                Autopilot::Off
            } else {
                Autopilot::Cruise
            };
            assert!(app.set_session_autopilot(0, Some(word)));
        }),
    ];
    for (i, (what, withhold)) in withheld.into_iter().enumerate() {
        let (mut app, _host) = hosted();
        withhold(&mut app, true);
        let t = 100_000 * (i as u64 + 1);
        on_act_at(&mut app, 0, Act::Continued, at(t0, t));
        let glass = app.chip_glass(0);
        assert_eq!(glass.lit, None, "{what}: no label");
        assert_eq!(
            glass.last.map(|(act, _)| act),
            Some(Act::Continued),
            "{what}: the trail keeps it"
        );
        assert_eq!(app.harness_acts_deadline(), None, "{what}: nothing armed");
        withhold(&mut app, false);
        on_act_at(&mut app, 0, Act::Approved, at(t0, t + 1000));
        assert_eq!(
            app.chip_glass(0).lit,
            Some(Act::Approved),
            "{what} lifted: the next act lights"
        );
    }

    // A label lit, then the switch off: the frame withholds it at once.
    let (mut app, _host) = hosted();
    on_act_at(&mut app, 0, Act::Approved, at(t0, 0));
    acts(&mut app, false);
    assert_eq!(app.chip_glass(0).lit, None);
    acts(&mut app, true);
    assert_eq!(app.chip_glass(0).lit, Some(Act::Approved));
}

/// THE TRAIL AS THE TITLE LISTS IT (§9.2.1), and the whole and short titles
/// with 0, 1 and 3 acts: newest first, each with the clock time it arrived;
/// an act from another local date with its weekday (`Thu 23:58`); one older
/// than a day left out. The whole title names the mode, says what it does,
/// then `last:`, then the clicks; the short title is the mode phrase alone
/// whatever the trail holds.
///
/// NEGATIVE CONTROL: read the day before, every act carries a weekday; read
/// a day and a minute after the oldest, it goes.
#[test]
fn the_trail_title_lists_clock_times_weekdays_and_never_a_day_old_act() {
    let t0 = Instant::now();
    let today = T0_UNIX.div_euclid(86_400);
    let mut g = SessionGlass::default();
    let whole = |g: &SessionGlass, now: i64| {
        autopilot_title(
            AutopilotView::Cruise,
            &g.chip(),
            &g.trail_text(now, today),
            Some(today),
        )
    };
    assert_eq!(g.trail_text(T0_UNIX, today), "");
    assert_eq!(
        whole(&g, T0_UNIX),
        (
            "autopilot: cruise \u{2014} keeps the work moving \u{b7} click: land \u{b7} \
             right-click: more"
                .to_owned(),
            "autopilot: cruise".to_owned()
        ),
        "no acts: v1's title, and the menu's line"
    );
    // Yesterday 23:58, then today 13:56 and 14:02 — read at 14:05.
    let at_unix = |unix: i64, ms: u64| Arrival {
        now: t0 + Duration::from_millis(ms),
        unix,
        offset_s: Some(0),
    };
    let midnight = today * 86_400;
    g.take(Act::Answered, at_unix(midnight - 120, 0), true);
    assert_eq!(
        whole(&g, T0_UNIX).0,
        format!(
            "autopilot: cruise \u{2014} keeps the work moving \u{b7} last: ? answered {} 23:58 \
             \u{b7} click: land \u{b7} right-click: more",
            Weekday::of_days(today - 1).short()
        ),
        "one act, from yesterday"
    );
    g.take(Act::Continued, at_unix(T0_UNIX - 540, 10), true);
    g.take(Act::Approved, at_unix(T0_UNIX - 180, 20), true);
    let day = Weekday::of_days(today - 1).short();
    assert_eq!(
        g.trail_text(T0_UNIX, today),
        format!("\u{2713} approved 14:02, \u{21bb} kept going 13:56, ? answered {day} 23:58")
    );
    let (title, short) = whole(&g, T0_UNIX);
    assert_eq!(
        title,
        format!(
            "autopilot: cruise \u{2014} keeps the work moving \u{b7} last: \u{2713} approved \
             14:02, \u{21bb} kept going 13:56, ? answered {day} 23:58 \u{b7} click: land \u{b7} \
             right-click: more"
        )
    );
    assert_eq!(short, "autopilot: cruise", "the mode phrase alone");
    // Short for every face, whatever the trail.
    for view in [
        AutopilotView::Cruise,
        AutopilotView::Land,
        AutopilotView::Off,
    ] {
        let (_, short) =
            autopilot_title(view, &g.chip(), &g.trail_text(T0_UNIX, today), Some(today));
        assert_eq!(
            short,
            crate::claude_lights::ChipFace::of(view, &g.chip()).phrase(&g.chip(), Some(today))
        );
    }
    // NEGATIVE CONTROLS: read on the day before, each act carries a day;
    // a day and a minute past the oldest, it is gone.
    assert!(g.trail_text(T0_UNIX, today - 1).starts_with(&format!(
        "\u{2713} approved {} 14:02",
        Weekday::of_days(today).short()
    )));
    let later = midnight - 120 + 86_400 + 60;
    assert_eq!(
        g.trail_text(later, today),
        "\u{2713} approved 14:02, \u{21bb} kept going 13:56"
    );
}

/// IDLE TO ZERO (§9.2.5): after the dwell the glass is v1's at rest but for
/// its trail — no label, nothing armed, and the footer's Autopilot term back
/// at its rest value — and `expire_harness_acts` with nothing lit reads no
/// clock.
///
/// CONTROL: during the dwell the deadline is the label's end and the term
/// carries the act.
#[test]
fn after_the_dwell_nothing_is_armed_and_the_term_is_at_rest() {
    let t0 = Instant::now();
    let (mut app, _host) = hosted();
    let lights = crate::claude_lights::WindowLights::default();
    let term = |app: &App| lights.autopilot_term(0, None, AutopilotView::Cruise, app.chip_glass(0));
    let rest = term(&app);
    on_act_at(&mut app, 0, Act::Approved, at(t0, 0));
    assert_ne!(term(&app), rest, "CONTROL: the label moves the term");
    assert_eq!(app.harness_acts_deadline(), Some(t0 + DWELL));
    app.expire_harness_acts_at(t0 + DWELL);
    assert_eq!(app.harness_acts_deadline(), None);
    assert_eq!(term(&app), rest, "at rest again, the trail kept");
    assert!(app.chip_glass(0).last.is_some());
    let _ = crate::work_counts::take_harness_clock_reads();
    app.expire_harness_acts();
    assert_eq!(
        crate::work_counts::take_harness_clock_reads(),
        0,
        "nothing lit: no clock"
    );
    // The session retired: its glass goes with it.
    app.forget_harness_glass(0);
    assert!(app.harness_glass.is_empty());
}

/// THE WAIT NEVER RE-ARMS THE WAKE THAT CAME (§9.2.3 item 6): what the loop
/// folds on its way to a wait is the NEXT instant the glass needs. A label
/// whose dwell ended is cleared, its footer posted, before the fold reads
/// the deadline, so the instant that woke the loop is not armed again,
/// already past (a second wake per label, measured in the lab as
/// `harness_acts` fires 2 and `past_deadline_arms` 1 for one label).
///
/// NEGATIVE CONTROL: the bare deadline, read as the fold read it before
/// the expiry, is the ended label's own instant, already past.
/// CONTROL: during the dwell the wait is the label's end, still ahead, and
/// the label stays lit.
#[test]
fn the_wait_never_rearms_the_label_end_that_woke_the_loop() {
    let (mut app, _host) = hosted();
    // CONTROL: lit now, the wait is its end, still ahead.
    let t0 = Instant::now();
    on_act_at(&mut app, 0, Act::Approved, at(t0, 0));
    assert_eq!(app.harness_acts_wait(), Some(t0 + DWELL));
    assert_eq!(app.chip_glass(0).lit, Some(Act::Approved));

    // A label lit ten seconds ago, its end passed: the loop woke for it.
    let (mut app, _host) = hosted();
    let t0 = Instant::now()
        .checked_sub(Duration::from_secs(10))
        .expect("a monotonic clock ten seconds past its origin");
    on_act_at(&mut app, 0, Act::Approved, at(t0, 0));
    let ended = app
        .harness_acts_deadline()
        .expect("NEGATIVE CONTROL: armed");
    assert!(
        ended <= Instant::now(),
        "NEGATIVE CONTROL: the bare deadline is the past instant that woke the loop"
    );
    assert_eq!(app.harness_acts_wait(), None, "nothing past is armed again");
    assert_eq!(app.chip_glass(0).lit, None, "the ended label went first");
    assert_eq!(
        app.chip_glass(0).last.map(|(act, _)| act),
        Some(Act::Approved)
    );
}

/// The task's face, told, is kept for the frame with its clock; told again
/// it is no move; told unknown over no trail it leaves no entry.
#[test]
fn the_task_face_is_kept_for_the_frame() {
    let mut app = App::headless_for_test();
    assert!(on_task(
        &mut app,
        7,
        Some(TaskFace::Landed),
        1_792_249_200_000
    ));
    assert_eq!(app.chip_glass(7).task, Some(TaskFace::Landed));
    assert!(!on_task(
        &mut app,
        7,
        Some(TaskFace::Landed),
        1_792_249_200_000
    ));
    assert!(on_task(&mut app, 7, None, 1_792_249_260_000));
    assert!(app.harness_glass.is_empty(), "unknown keeps no entry");
    assert_eq!(app.chip_glass(7), ChipGlass::default());
    on_task(&mut app, 8, Some(TaskFace::Spent), 0);
    app.forget_harness_glass(8);
    assert!(app.harness_glass.is_empty());
    assert_eq!(app.harness_acts_deadline(), None);
}

/// A TASK'S FACE IS STAMPED WITH THE WALL CLOCK (unix ms, as the timeline
/// keeps it): told through the window's one door for harness facts, the
/// face's time is the wall's now, so its clock time on the chip and the page
/// (`landed 17:42`) is the time it landed — never the process's age read as
/// a time of 1970.
#[test]
fn a_task_face_is_stamped_with_the_wall_clock() {
    let wall_ms = || {
        i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("after the epoch")
                .as_millis(),
        )
        .expect("ms")
    };
    let mut app = App::headless_for_test();
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    let sid = ctx.self_id.as_str().to_string();
    let before = wall_ms();
    app.apply_harness_fact(&sid, crate::HarnessFact::Task(Some(TaskFace::Landed)));
    let after = wall_ms();
    let (face, at_ms) = ctx.timeline.lock().unwrap().task_face().expect("a face");
    assert_eq!(face, TaskFace::Landed);
    assert!(
        (before..=after).contains(&at_ms),
        "{at_ms} between {before} and {after}"
    );
    assert_eq!(
        app.chip_glass(0).task_at,
        crate::harness_glass::Stamp::at(at_ms.div_euclid(1000)),
        "the chip's clock and day are the wall's"
    );
}

/// A STAMP NAMES ITS DAY SO IT CANNOT BE READ AS ANOTHER (§10.2.3, item 3;
/// the honest chip's review, finding 3): read against its own day, or with
/// no day read, the bare clock; within the six days before `today` its
/// weekday (`Sat 17:58` on a Friday, six days back); a week or more back its
/// date (`11 Oct 17:58` on the Friday after, where the weekday would be
/// today's own), with the year where that differs; and after `today` (a
/// clock set back) its date, never a weekday read as past. Spoken, spelled
/// out (`on Saturday at 17:58`, `on 11 October at 17:58`).
///
/// NEGATIVE CONTROL: a weekday alone at seven days back reads `Fri` on a
/// Friday — today's own name, the misreading the date replaces.
#[test]
fn a_stamp_names_its_day_so_it_cannot_be_read_as_another() {
    // 2024-10-11, a Friday.
    const FRI: i64 = 20_007;
    let stamp = |day: i64| Stamp {
        clock: Clock::of_minute(17 * 60 + 58),
        day,
    };
    for (day, today, words, spoken) in [
        (FRI, Some(FRI), "17:58", "at 17:58"),
        (FRI, None, "17:58", "at 17:58"),
        (FRI, Some(FRI + 1), "Fri 17:58", "on Friday at 17:58"),
        (FRI - 6, Some(FRI), "Sat 17:58", "on Saturday at 17:58"),
        (FRI, Some(FRI + 7), "11 Oct 17:58", "on 11 October at 17:58"),
        (FRI, Some(FRI + 8), "11 Oct 17:58", "on 11 October at 17:58"),
        (
            FRI,
            Some(FRI + 90),
            "11 Oct 2024 17:58",
            "on 11 October 2024 at 17:58",
        ),
        (FRI + 1, Some(FRI), "12 Oct 17:58", "on 12 October at 17:58"),
    ] {
        assert_eq!(stamp(day).words(today), words, "{day} {today:?}");
        assert_eq!(stamp(day).spoken(today), spoken, "{day} {today:?}");
    }
    // NEGATIVE CONTROL: the weekday rule alone, a week back, names today.
    assert_eq!(Weekday::of_days(FRI), Weekday::of_days(FRI + 7));
    assert_eq!(Weekday::of_days(FRI).short(), "Fri");
}
