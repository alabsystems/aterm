// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The park: Ink's rewrite observed as two sync brackets.

use super::*;

/// S6b: five keys, each echoed as a park to col 2 and a return to the
/// row's end plus one, ~60 ms apart. One typed echo per key: every
/// typed cell lit, the prompt cells dark, nothing refused, nothing
/// forgotten, the return rows judged from the park's origin.
#[test]
fn an_ink_rewrite_observed_as_park_and_return_is_one_typed_echo_per_key() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((11, 2)), t0, &c, g, &mut out);
    // k1: a plain echo.
    glow.note_typed(t0);
    glow.tick(
        Some((11, 3)),
        t0 + Duration::from_millis(10),
        &c,
        g,
        &mut out,
    );
    // k2..k5: park, then return.
    let mut end = 3u16;
    let mut t = t0;
    for _ in 0..4 {
        t += Duration::from_millis(120);
        glow.note_typed(t);
        glow.tick(
            Some((11, 2)),
            t + Duration::from_millis(10),
            &c,
            g,
            &mut out,
        );
        end += 1;
        glow.tick(
            Some((11, end)),
            t + Duration::from_millis(70),
            &c,
            g,
            &mut out,
        );
    }
    let last = t + Duration::from_millis(70);
    let lit = v2_cols(&glow, 11);
    assert_eq!(
        lit,
        (2..=6u16).collect(),
        "every typed cell lit, the prompt cells dark"
    );
    let tally = glow.admission_tally();
    assert_eq!(
        (tally.declined, tally.last_decline_reason),
        (0, None),
        "nothing refused"
    );
    let rows = ring_rows(&glow);
    assert_eq!(
        rows,
        vec![
            ("licensed", "key", (11, 2), (11, 3)),
            ("licensed", "key", (11, 3), (11, 4)),
            ("licensed", "key", (11, 4), (11, 5)),
            ("licensed", "key", (11, 5), (11, 6)),
            ("licensed", "key", (11, 6), (11, 7)),
        ],
        "one row per key, each from the park's origin"
    );
    assert_eq!(
        glow.typed_credits_within(last),
        0,
        "every press spent by its own cell"
    );
    let in_flight = glow.in_flight_tally();
    assert_eq!(
        (
            in_flight.forgotten,
            in_flight.park_returns,
            in_flight.park_flushed
        ),
        (0, 4, 0)
    );
    assert_eq!(
        glow.v2.caret_mirror(),
        (11, 7),
        "the mirror follows the returns"
    );
    assert_eq!(bridged(&glow), 0);
}

/// S6c: three keys typed into a stall, the rewrite arriving with no
/// stamp fresh — the park is held on the in-flight pool (nothing
/// forgotten), and its return is the batch's echo from the origin,
/// licensed by the pool alone. Control: the same park with an EMPTY
/// pool is refused at once, as today.
#[test]
fn an_ink_park_after_a_stall_is_held_on_the_in_flight_pool_and_its_return_sweeps_the_batch() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let pre = pre_roll(&mut glow, 11, t0, &c, g);
    let last = stalled_keys(&mut glow, 3, pre);
    let park_at = last + Duration::from_millis(350);
    glow.tick(Some((11, 2)), park_at, &c, g, &mut out);
    assert_eq!(
        glow.admission_tally().declined,
        0,
        "the park is held, not refused"
    );
    assert_eq!(glow.in_flight_tally().forgotten, 0, "…and forgets nothing");
    assert_eq!(glow.typed_credits_within(park_at), 3);
    let ret_at = park_at + Duration::from_millis(40);
    glow.tick(Some((11, 8)), ret_at, &c, g, &mut out);
    let tally = glow.admission_tally();
    assert_eq!((tally.declined, tally.last_decline_reason), (0, None));
    let last_row = glow
        .admission_log()
        .last()
        .map(|r| (r.licence, r.origin, r.target))
        .expect("a ring row");
    assert_eq!(
        last_row,
        (AdmissionRecord::LICENCE_IN_FLIGHT, (11, 5), (11, 8)),
        "the return is the batch's echo from the origin, on the pool"
    );
    assert!(
        dark_in(&glow, 11, 2..8).is_empty(),
        "cols 2..7 lit; dark: {:?}",
        dark_in(&glow, 11, 2..8)
    );
    assert_eq!(glow.in_flight_tally().park_returns, 1);
    assert_eq!(bridged(&glow), 0);

    // CONTROL: the same park over an empty pool is refused at once.
    let mut empty = CursorGlow::default();
    let pre = pre_roll(&mut empty, 11, t0, &c, g);
    empty.tick(
        Some((11, 2)),
        pre + Duration::from_millis(400),
        &c,
        g,
        &mut out,
    );
    assert_eq!(
        empty.admission_tally().last_decline_reason,
        Some(CursorGlow::DECLINE_NO_FRESH_HINT),
        "a keyless backward hop with nothing in flight is refused as today"
    );
    assert_eq!(empty.in_flight_tally().park_flushed, 0);
}

/// A park with NO return flushes as today's verdict at the park's own
/// clock: the stamp variant lands its `licence=key` row from the origin
/// to the landing (a two-cell jump), consumes the park's stamp and moves
/// the mirror to the landing; the in-flight variant is refused and the
/// pool forgotten at the flush. A keyless +2 after either is refused.
#[test]
fn a_park_with_no_return_flushes_as_todays_verdict() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let pre = pre_roll(&mut glow, 11, t0, &c, g);
    let rows_before = ring_rows(&glow).len();
    let k = pre + Duration::from_millis(85);
    glow.note_typed(k);
    let park_at = k + Duration::from_millis(10);
    glow.tick(Some((11, 3)), park_at, &c, g, &mut out);
    assert_eq!(ring_rows(&glow).len(), rows_before, "held: no ring row yet");
    assert!(
        glow.move_licensed(park_at),
        "…and the stamp not yet consumed"
    );
    let quiet = park_at + Duration::from_millis(300);
    glow.tick(Some((11, 3)), quiet, &c, g, &mut out);
    let rows = ring_rows(&glow);
    assert_eq!(
        rows.last(),
        Some(&("licensed", "key", (11, 5), (11, 3))),
        "the flush judges the park from its origin: {rows:?}"
    );
    assert_eq!(glow.v2.caret_mirror(), (11, 3));
    assert!(
        !glow.move_licensed(quiet),
        "the park's stamp was consumed at the flush"
    );
    assert_eq!(glow.in_flight_tally().park_flushed, 1);
    glow.tick(
        Some((11, 5)),
        quiet + Duration::from_millis(50),
        &c,
        g,
        &mut out,
    );
    assert_eq!(
        glow.admission_tally().last_decline_reason,
        Some(CursorGlow::DECLINE_NO_FRESH_HINT),
        "a keyless +2 after the flush is refused"
    );

    // The in-flight variant: no stamp, one press, no return.
    let mut stalled = CursorGlow::default();
    let pre = pre_roll(&mut stalled, 11, t0, &c, g);
    stalled.note_typed(pre + Duration::from_millis(85));
    let park_at = pre + Duration::from_millis(500);
    stalled.tick(Some((11, 3)), park_at, &c, g, &mut out);
    assert_eq!(stalled.in_flight_tally().forgotten, 0, "held");
    stalled.tick(
        Some((11, 3)),
        park_at + Duration::from_millis(300),
        &c,
        g,
        &mut out,
    );
    assert_eq!(
        stalled.admission_tally().last_decline_reason,
        Some(CursorGlow::DECLINE_NO_FRESH_HINT),
        "refused at the flush"
    );
    assert_eq!(
        stalled.in_flight_tally().forgotten,
        1,
        "…and the pool forgotten"
    );
    assert_eq!(stalled.in_flight_tally().park_flushed, 1);
}

/// **A ONE-CELL PARK'S FLUSH DRAINS NOTHING** (2026-09-21): parity
/// between the two floors. A key typed, the caret observed ONE column
/// left of where it stood (a repaint parking on the trailing space it
/// has just drawn), held as a park, and flushed after the window: the
/// flush judges it `typing` for one cell and hands v2 a same-row
/// backward `Typed` move with no erase behind it. To the host that is
/// plain typing (`classify_move` admits a re-anchor only past two
/// cells, its `raw_dist > 2`), and it must not be a re-anchor to the
/// ribbon either: before `ribbon::RE_ANCHOR_MIN_CELLS` the ribbon
/// `retract_suffix`-ed exactly the last laid cell on it. The band's
/// cells all stand, none leaving,
/// the mirror at the landing; the control at three cells is the
/// re-anchor as before. This trigger — the flushed one-cell park — is
/// pinned HERE, at unit level; it was not measured at the host seam,
/// and no host-seam take of the owner's gesture reproduced the dark
/// boundary space through it.
///
/// RED before the floor: `leaving=[4]`.
#[test]
fn a_one_cell_park_flushed_as_typing_drains_no_cell_of_the_band() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let ms = Duration::from_millis;
    let leaving = |glow: &CursorGlow| {
        glow.v2
            .ribbon()
            .cells()
            .iter()
            .filter(|l| l.row == 11 && l.leaving())
            .map(|l| l.col)
            .collect::<Vec<_>>()
    };
    for (back, drains) in [(1u16, false), (2, false), (3, true)] {
        let mut out = Vec::new();
        let mut glow = CursorGlow::default();
        let t0 = Instant::now();
        let pre = pre_roll(&mut glow, 11, t0, &c, g);
        assert_eq!(
            v2_cols(&glow, 11).into_iter().collect::<Vec<_>>(),
            vec![2, 3, 4],
            "{back} back: the pre-roll's band"
        );
        let k = pre + ms(85);
        glow.note_typed(k);
        let park_at = k + ms(10);
        let landing = 5 - back;
        glow.tick(Some((11, landing)), park_at, &c, g, &mut out);
        assert!(glow.held_park.is_some(), "{back} back: held");
        assert!(leaving(&glow).is_empty(), "{back} back: nothing while held");
        // Silence past the window: the flush, today's verdict.
        let quiet = park_at + past_park_window();
        glow.tick(Some((11, landing)), quiet, &c, g, &mut out);
        assert_eq!(glow.in_flight_tally().park_flushed, 1, "{back} back");
        assert_eq!(
            glow.v2.caret_mirror(),
            (11, landing),
            "{back} back: the mirror follows the flush"
        );
        if drains {
            assert!(
                !leaving(&glow).is_empty(),
                "at the host's re-anchor floor the flush still drains the row"
            );
        } else {
            assert!(
                leaving(&glow).is_empty(),
                "a {back}-cell park's flush drained the band: leaving={:?}",
                leaving(&glow)
            );
            assert_eq!(
                v2_cols(&glow, 11).into_iter().collect::<Vec<_>>(),
                vec![2, 3, 4],
                "{back} back: every cell of the band stands"
            );
        }
    }
}

/// A park followed by a Backspace flushes BEFORE the erase: the park's
/// `Move` precedes the `Erase` in v2's event order, and the bs-paired
/// retreat that follows keeps today's classification. A scroll does
/// NOT flush it: the park rides with its row, like the band twin, and
/// is judged there.
#[test]
fn a_park_followed_by_a_backspace_flushes_before_the_erase() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let pre = pre_roll(&mut glow, 11, t0, &c, g);
    let k = pre + Duration::from_millis(85);
    glow.note_typed(k);
    let park_at = k + Duration::from_millis(10);
    glow.tick(Some((11, 3)), park_at, &c, g, &mut out);
    let bs = park_at + Duration::from_millis(50);
    glow.note_backspace(bs);
    let kinds: Vec<&str> = glow
        .v2
        .pending_events()
        .iter()
        .map(|(ev, _)| match ev {
            rk::Event::Move { .. } => "move",
            rk::Event::Erase => "erase",
            _ => "other",
        })
        .collect();
    assert_eq!(
        kinds,
        vec!["move", "erase"],
        "the park's move precedes the erase"
    );
    assert_eq!(glow.in_flight_tally().park_flushed, 1);
    glow.tick(
        Some((11, 2)),
        bs + Duration::from_millis(10),
        &c,
        g,
        &mut out,
    );
    assert_eq!(
        ring_rows(&glow).last(),
        Some(&("licensed", "key", (11, 3), (11, 2))),
        "the bs-paired retreat is judged as today"
    );

    // A scroll: the park rides with its row and is judged there
    // (`a_held_park_rides_a_band_move_or_a_scroll_and_its_return_lands_on_the_moved_row`
    // has the return and the off-the-top flush).
    let mut scrolled = CursorGlow::default();
    let pre = pre_roll(&mut scrolled, 11, t0, &c, g);
    scrolled.note_typed(pre + Duration::from_millis(85));
    let park_at = pre + Duration::from_millis(95);
    scrolled.tick(Some((11, 3)), park_at, &c, g, &mut out);
    scrolled.note_scroll(1);
    assert_eq!(scrolled.in_flight_tally().park_flushed, 0, "not flushed");
    assert_eq!(
        scrolled.held_park.map(|p| (p.row, p.origin, p.landing)),
        Some((10, 5, 3)),
        "…the park rides the scroll with its row"
    );
    scrolled.tick(Some((10, 3)), park_at + past_park_window(), &c, g, &mut out);
    assert_eq!(scrolled.in_flight_tally().park_flushed, 1);
    assert_eq!(
        ring_rows(&scrolled).last(),
        Some(&("licensed", "key", (10, 5), (10, 3))),
        "judged on the moved row"
    );
}

/// THE BOX-GROWTH WRAP re-anchors identically under the park rule:
/// Claude Code's input box grows a row UP, so the caret's terminal row
/// stays constant and the wrap is a same-row hop far to the left, then
/// typing from the new column. The park flushes as the re-anchor (only
/// the landing lit), the +1 follows, the ring rows are in order, nothing
/// forgotten, nothing on the in-flight licence — and the cat's fold
/// pulse still arrives, dated at the wrap.
#[test]
fn a_box_growth_wrap_reanchors_identically_under_the_park_rule() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((5, 36)), t0, &c, g, &mut out);
    let k1 = t0 + Duration::from_millis(5);
    glow.note_typed(k1);
    let wrap_at = t0 + Duration::from_millis(10);
    glow.tick(Some((5, 3)), wrap_at, &c, g, &mut out);
    let k2 = t0 + Duration::from_millis(100);
    glow.note_typed(k2);
    glow.tick(Some((5, 4)), k2 + Duration::from_millis(8), &c, g, &mut out);
    let lit = v2_cols(&glow, 5);
    assert!(lit.contains(&3), "the landing is lit: {lit:?}");
    assert!(
        !(4..=36u16).any(|col| lit.contains(&col)),
        "nothing between the landing and the launch: {lit:?}"
    );
    assert_eq!(
        ring_rows(&glow),
        vec![
            ("licensed", "key", (5, 36), (5, 3)),
            ("licensed", "key", (5, 3), (5, 4)),
        ]
    );
    let in_flight = glow.in_flight_tally();
    assert_eq!(
        (
            in_flight.forgotten,
            in_flight.licensed,
            in_flight.park_flushed
        ),
        (0, 0, 1)
    );
    assert_eq!(
        glow.take_cursor_cat_motion_pulse(),
        Some(CursorCatMotionPulse {
            at: wrap_at,
            kind: CursorCatMotionKind::FoldForward,
        }),
        "the cat turns the corner it really turned"
    );
    assert_eq!(glow.v2.caret_mirror(), (5, 4));
}

/// A SHORT RETREAT beside a fresh stamp with no return keeps the
/// hide-bridge verdict: one cell flushes as `typing`, two as a jump,
/// each a `licence=key` row from the origin, the mirror at the landing.
#[test]
fn a_short_retreat_beside_a_fresh_stamp_keeps_the_hide_bridge_verdict() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let t0 = Instant::now();
    for retreat in [1u16, 2] {
        let mut glow = CursorGlow::default();
        let pre = pre_roll(&mut glow, 11, t0, &c, g);
        let k = pre + Duration::from_millis(85);
        glow.note_typed(k);
        let park_at = k + Duration::from_millis(10);
        glow.tick(Some((11, 5 - retreat)), park_at, &c, g, &mut out);
        glow.tick(
            Some((11, 5 - retreat)),
            park_at + Duration::from_millis(300),
            &c,
            g,
            &mut out,
        );
        assert_eq!(
            ring_rows(&glow).last(),
            Some(&("licensed", "key", (11, 5), (11, 5 - retreat))),
            "a {retreat}-cell retreat is judged as today"
        );
        assert_eq!(glow.v2.caret_mirror(), (11, 5 - retreat));
        assert_eq!(glow.admission_tally().declined, 0);
        assert_eq!(glow.in_flight_tally().park_flushed, 1);
    }
}

/// A HELD PARK RIDES A BAND MOVE. Codex prints a line every 10–230 ms
/// while the hand types, and Ink's
/// park/return gap is ~40 ms: a streamed line between the two slides
/// the composer row one down (or, with the transcript archiving, one
/// up). `note_band_move` moved `last`, `last_visible`, the sparks and
/// the v2 mirror but left `held_park` on the OLD row, so the return
/// failed `pr == p.row`: the park flushed as a licensed `Move` on the
/// abandoned row — a streamed program line — laying a stray cell
/// there, its stamp consumed, and the real return on the moved row was
/// refused `no-fresh-hint` with the key's cell dark (S6b re-opened).
/// The park now translates under the [`band_row`] law like every other
/// row-addressed member — inside the band it rides `delta`, carried
/// past the band's edge it is judged first in the pre-move space, as
/// `note_scroll` judges it — so the return is recognised on the moved
/// row where the mirror already sits. Both directions.
///
/// A HELD PARK RIDES A SCROLL TOO. The band rows ride the park with its
/// row; before this fix a scroll flushed it before the translation instead, while every
/// other row-addressed member — `last`, `last_visible`, the sparks,
/// the v2 mirror, the buffered events — rides both edges. Under
/// Claude Code on the main screen a transcript line printed inside
/// Ink's ~40 ms park/rewrite gap is a scroll: the park was flushed as
/// a typed re-anchor at its own clock (the key's stamp spent on the
/// prompt cell) and the real return on the moved row was refused
/// `no-fresh-hint`, the key's cell dark. The park now rides the
/// scroll (`row - rows`), flushed only when carried off the top — the
/// band law and the mirror's `translate_scroll`.
///
/// RED before the fix: `park_flushed=1` at the scroll, the return
/// `("no-fresh-hint","none",(10,2),(10,6))`, col 5 dark on row 10.
#[test]
fn a_held_park_rides_a_band_move_or_a_scroll_and_its_return_lands_on_the_moved_row() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let t0 = Instant::now();
    /// What moved, the move, and the row the park and the caret land on.
    type Ride = (&'static str, fn(&mut CursorGlow), u16);
    let rides: [Ride; 3] = [
        ("band +1", |glow| glow.note_band_move(0, 20, 1), 12),
        ("band -1", |glow| glow.note_band_move(0, 20, -1), 10),
        ("scroll", |glow| glow.note_scroll(1), 10),
    ];
    for (what, shift, moved) in rides {
        let mut glow = CursorGlow::default();
        let pre = pre_roll(&mut glow, 11, t0, &c, g);
        let k = pre + Duration::from_millis(85);
        glow.note_typed(k);
        let park_at = k + Duration::from_millis(10);
        glow.tick(Some((11, 2)), park_at, &c, g, &mut out);
        assert!(glow.held_park.is_some(), "the park is held ({what})");
        assert_eq!(glow.v2.caret_mirror(), (11, 5), "the mirror at the origin");
        shift(&mut glow);
        assert_eq!(
            glow.held_park.map(|p| (p.row, p.origin, p.landing)),
            Some((moved, 5, 2)),
            "the park rides the move with its row ({what})"
        );
        assert_eq!(
            glow.in_flight_tally().park_flushed,
            0,
            "…and is not flushed by it ({what})"
        );
        let ret_at = park_at + Duration::from_millis(40);
        glow.tick(Some((moved, 6)), ret_at, &c, g, &mut out);
        assert_eq!(
            ring_rows(&glow).last(),
            Some(&("licensed", "key", (moved, 5), (moved, 6))),
            "the return is judged from the park's origin on the moved row ({what}): {:?}",
            ring_rows(&glow)
        );
        let in_flight = glow.in_flight_tally();
        assert_eq!(
            (
                in_flight.park_returns,
                in_flight.park_flushed,
                in_flight.forgotten
            ),
            (1, 0, 0),
            "{what}"
        );
        assert_eq!(
            glow.admission_tally().declined,
            0,
            "nothing refused ({what})"
        );
        assert!(
            v2_cols(&glow, 11).is_empty(),
            "no stray light on the row the move left behind ({what}): {:?}",
            v2_cols(&glow, 11)
        );
        assert_eq!(
            v2_cols(&glow, moved),
            (2..=5u16).collect(),
            "the moved row's typed cells lit, the key's own cell among them ({what})"
        );
        assert_eq!(glow.v2.caret_mirror(), (moved, 6), "{what}");
    }
    // CARRIED PAST THE BAND'S EDGE, or OFF THE TOP: no honest row for the
    // park — it is judged first, in the pre-move space, exactly as before
    // (`a_park_with_no_return_flushes_as_todays_verdict`'s verdict,
    // emitted at the move).
    /// What carried it, the row the park is held on, and the move.
    type Carry = (&'static str, u16, fn(&mut CursorGlow));
    let carried: [Carry; 2] = [
        ("out of the band", 11, |glow| glow.note_band_move(11, 11, 1)),
        ("off the top", 0, |glow| glow.note_scroll(1)),
    ];
    for (what, row, shift) in carried {
        let mut glow = CursorGlow::default();
        let pre = pre_roll(&mut glow, row, t0, &c, g);
        let k = pre + Duration::from_millis(85);
        glow.note_typed(k);
        let park_at = k + Duration::from_millis(10);
        glow.tick(Some((row, 2)), park_at, &c, g, &mut out);
        assert!(glow.held_park.is_some(), "the park is held ({what})");
        shift(&mut glow);
        assert!(glow.held_park.is_none(), "carried {what}: flushed");
        assert_eq!(glow.in_flight_tally().park_flushed, 1, "{what}");
        assert_eq!(
            ring_rows(&glow).last(),
            Some(&("licensed", "key", (row, 5), (row, 2))),
            "judged in the pre-move space ({what}): {:?}",
            ring_rows(&glow)
        );
    }
}

/// The host's pre-roll with the PRINT ANCHOR fed on every echo tick,
/// exactly as `app_render` feeds it before each glow tick: "I t" typed
/// and echoed per key from `(row, 2)`, the caret at `(row, 5)`. Returns
/// the clock of the last echo and the anchor sequence it reached.
fn pre_roll_anchored(
    glow: &mut CursorGlow,
    row: u16,
    t0: Instant,
    c: &GlowConfig,
    g: Geom,
) -> (Instant, u64) {
    let mut out = Vec::new();
    glow.observe_print_anchor(Some((row, 2, 1)));
    glow.tick(Some((row, 2)), t0, c, g, &mut out);
    let mut t = t0;
    let mut seq = 1u64;
    for col in 2..5u16 {
        t += Duration::from_millis(85);
        glow.note_typed(t);
        seq += 1;
        glow.observe_print_anchor(Some((row, col + 1, seq)));
        glow.tick(
            Some((row, col + 1)),
            t + Duration::from_millis(3),
            c,
            g,
            &mut out,
        );
    }
    (t + Duration::from_millis(3), seq)
}

/// THE RETURN SEEN ON THE ANCHORED LANE. Ink's second bracket — hide,
/// rewrite the row, show — can be sampled
/// mid-bracket: the caret hidden and the print anchor already advanced
/// from the park's ORIGIN to the row's new end. That is the return,
/// seen on the other lane. `spawn` used to flush the park there
/// (`(11,5)->(11,2)` as a typed re-anchor: the prompt cell lit, the
/// stamp and credit spent), refuse the key's own anchored echo
/// `no-fresh-hint`, and leave the mirror parked at the landing — so the
/// show frame's `(11,2)->(11,6)` was a declined relocation, the key's
/// glyph dark, and the next key typed into a stall replayed its `Typed`
/// on the prompt cell. The anchored lane now recognises the return on
/// the same terms as the visible one and judges `origin -> end` on the
/// key's own stamp.
///
/// Three shapes: the hidden frame WITH the print in (RED before the
/// fix: `("no-fresh-hint","none",(11,5),(11,6))`, lit {1,2,3,4}, the
/// mirror at (11,2)); the hidden frame without it (the hidden-boundary
/// shape, a control); the whole bracket seen at once on the visible lane (a
/// control). Each ends with every typed cell lit, the prompt cell
/// dark, the mirror at the row's end, nothing flushed and nothing
/// forgotten.
#[test]
fn a_held_parks_return_is_recognised_on_the_anchored_lane() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let ms = Duration::from_millis;
    for shape in ["hidden-with-print", "hidden-without-print", "visible-whole"] {
        let mut out = Vec::new();
        let mut glow = CursorGlow::default();
        let t0 = Instant::now();
        let (pre, mut seq) = pre_roll_anchored(&mut glow, 11, t0, &c, g);
        let k = pre + ms(85);
        glow.note_typed(k);
        let park_at = k + ms(10);
        glow.tick(Some((11, 2)), park_at, &c, g, &mut out);
        assert!(glow.held_park.is_some(), "{shape}: the park is held");
        match shape {
            "hidden-with-print" => {
                seq += 1;
                glow.observe_print_anchor(Some((11, 6, seq)));
                glow.tick(None, park_at + ms(15), &c, g, &mut out);
                assert_eq!(
                    ring_rows(&glow).last(),
                    Some(&("licensed", "key", (11, 5), (11, 6))),
                    "{shape}: the anchored lane judged the return from the origin: {:?}",
                    ring_rows(&glow)
                );
                assert_eq!(glow.v2.caret_mirror(), (11, 6), "{shape}");
                glow.tick(Some((11, 6)), park_at + ms(30), &c, g, &mut out);
            }
            "hidden-without-print" => {
                glow.tick(None, park_at + ms(15), &c, g, &mut out);
                seq += 1;
                glow.observe_print_anchor(Some((11, 6, seq)));
                glow.tick(Some((11, 6)), park_at + ms(30), &c, g, &mut out);
            }
            _ => {
                seq += 1;
                glow.observe_print_anchor(Some((11, 6, seq)));
                glow.tick(Some((11, 6)), park_at + ms(40), &c, g, &mut out);
            }
        }
        let lit = v2_cols(&glow, 11);
        let dark = dark_in(&glow, 11, 2..6);
        assert!(
            dark.is_empty(),
            "{shape}: dark={dark:?} ring={:?}",
            ring_rows(&glow)
        );
        assert!(!lit.contains(&1), "{shape}: the prompt cell lit: {lit:?}");
        assert_eq!(glow.v2.caret_mirror(), (11, 6), "{shape}");
        let tally = glow.in_flight_tally();
        assert_eq!(
            (tally.park_returns, tally.park_flushed, tally.forgotten),
            (1, 0, 0),
            "{shape}: {tally:?} ring={:?}",
            ring_rows(&glow)
        );
        assert_eq!(glow.admission_tally().declined, 0, "{shape}");
    }
}

/// THE SAME-END RETURN. A repaint that carries no new glyph — a spinner
/// or status frame between the park and the
/// key's echo — rewrites the row to its OLD end: `landing -> origin`,
/// zero cells. Not a return (`cc > p.origin` fails), it fell to the
/// flush: a typed re-anchor on the key's stamp (the prompt cell left
/// of the landing lit, the stamp popped, a credit spent), the forward
/// hop refused `no-fresh-hint`, and the key's real `+1` from the origin
/// refused too — its cell dark. The two moves cancel geometrically and
/// the mirror never left the origin, so the park is now dropped
/// silently: nothing consumed, no `Move`, no ring row; the stamp
/// survives for the echo it belongs to.
///
/// RED before the fix: `dark=[5] stray_prompt_cell_lit=true flushed=1`,
/// ring `licensed key (3,5)->(3,2)`, `no-fresh-hint (3,2)->(3,5)`,
/// `no-fresh-hint (3,5)->(3,6)`.
#[test]
fn a_park_returning_to_its_own_origin_is_not_a_re_anchor() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    three_visible_echoes(&mut glow, t0, &mut out);
    glow.note_typed(at(400));
    glow.tick(Some((3, 2)), at(420), &c, g, &mut out);
    assert!(glow.held_park.is_some(), "the park is held");
    let rows_at_park = ring_rows(&glow).len();
    // The spinner's repaint: the row rewritten to its old end.
    glow.tick(Some((3, 5)), at(460), &c, g, &mut out);
    assert!(glow.held_park.is_none(), "the park is dropped");
    assert_eq!(
        ring_rows(&glow).len(),
        rows_at_park,
        "no ring row for a park that cancelled itself: {:?}",
        ring_rows(&glow)
    );
    assert_eq!(
        glow.v2.caret_mirror(),
        (3, 5),
        "the mirror never left the origin"
    );
    assert!(
        glow.type_hint
            .any_fresh(at(460), CursorGlow::TYPE_HINT_FRESH),
        "the key's stamp survives for its own echo"
    );
    assert_eq!(glow.typed_credits_within(at(460)), 1);
    // The key's own echo.
    glow.tick(Some((3, 6)), at(520), &c, g, &mut out);
    assert_eq!(
        ring_rows(&glow).last(),
        Some(&("licensed", "key", (3, 5), (3, 6))),
        "{:?}",
        ring_rows(&glow)
    );
    let lit = v2_cols(&glow, 3);
    let dark = dark_in(&glow, 3, 2..6);
    assert!(dark.is_empty(), "dark={dark:?}");
    assert!(!lit.contains(&1), "the prompt cell lit: {lit:?}");
    let tally = glow.in_flight_tally();
    assert_eq!((tally.park_flushed, tally.forgotten), (0, 0), "{tally:?}");
    assert_eq!(glow.admission_tally().declined, 0);
    // …and a stalled key's rewrite (held on the pool, no stamp fresh)
    // to its own end is dropped the same way, the pool kept.
    let mut glow = CursorGlow::default();
    three_visible_echoes(&mut glow, t0, &mut out);
    glow.note_typed(at(400));
    glow.tick(Some((3, 2)), at(800), &c, g, &mut out);
    assert!(
        glow.held_park.is_some(),
        "held on the pool: {:?}",
        ring_rows(&glow)
    );
    glow.tick(Some((3, 5)), at(840), &c, g, &mut out);
    assert!(glow.held_park.is_none());
    assert_eq!(glow.typed_credits_within(at(840)), 1, "the pool is kept");
    glow.tick(Some((3, 6)), at(900), &c, g, &mut out);
    assert_eq!(
        ring_rows(&glow).last(),
        Some(&("licensed", "inflight", (3, 5), (3, 6))),
        "{:?}",
        ring_rows(&glow)
    );
    assert!(dark_in(&glow, 3, 2..6).is_empty());
    assert!(!v2_cols(&glow, 3).contains(&1));
}

/// A PARK FLUSHED AFTER ITS WINDOW SWEEPS NO PROMPT CELL. Ink's park
/// and rewrite are two brackets ~40 ms apart,
/// but under a stall the rewrite can come after the 0.25 s park
/// window: the park is then flushed as a typed re-anchor, and the
/// re-anchor's landing sweep (`re_anchor_lays_landing`, for the
/// box-growth wrap whose landing cell IS the wrapped glyph) lit the
/// cell left of the park landing — for an Ink park to the input's
/// start, the PROMPT cell, with no keystroke behind it. The park now
/// remembers whether a glyph sat at its landing cell when it was
/// held (the host's row probe), and the flush sweeps the landing only
/// then. The verdict itself is unchanged: the credit is spent, the
/// stamp popped, the ring row written, and the late return refused.
///
/// The row probe is fed before every tick as `app_render` feeds it.
/// RED before the fix: `lit {1,2,3,4}` after the flush — cell 1, the
/// prompt, lit. The control parks onto a program glyph (`ab` printed
/// left of the typing) and still sweeps it, as the box-growth wrap's
/// fixtures do.
#[test]
fn a_park_flushed_after_its_window_lights_no_blank_landing() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let ms = Duration::from_millis;
    for (prefix, swept) in [("> ", false), ("ab", true)] {
        let mut out = Vec::new();
        let mut glow = CursorGlow::default();
        let t0 = Instant::now();
        let mut row: Vec<char> = prefix.chars().collect();
        let probe = |glow: &mut CursorGlow, row: &[char], caret: u16, at: Instant| {
            glow.observe_row(11, caret, row, at);
        };
        probe(&mut glow, &row, 2, t0);
        glow.tick(Some((11, 2)), t0, &c, g, &mut out);
        let mut t = t0;
        for (i, ch) in ['I', ' ', 't'].into_iter().enumerate() {
            t += ms(85);
            glow.note_typed(t);
            row.push(ch);
            let col = 3 + i as u16;
            probe(&mut glow, &row, col, t + ms(3));
            glow.tick(Some((11, col)), t + ms(3), &c, g, &mut out);
        }
        assert_eq!(glow.spawns(), 3, "{prefix:?}: the pre-roll");
        let k = t + ms(88);
        glow.note_typed(k);
        // The park, the row intact.
        let park_at = k + ms(10);
        probe(&mut glow, &row, 2, park_at);
        glow.tick(Some((11, 2)), park_at, &c, g, &mut out);
        assert!(glow.held_park.is_some(), "{prefix:?}: held");
        // Silence past the window: the flush.
        let quiet = park_at + past_park_window();
        probe(&mut glow, &row, 2, quiet);
        glow.tick(Some((11, 2)), quiet, &c, g, &mut out);
        assert_eq!(
            ring_rows(&glow).last(),
            Some(&("licensed", "key", (11, 5), (11, 2))),
            "{prefix:?}: today's verdict, emitted at the flush: {:?}",
            ring_rows(&glow)
        );
        assert_eq!(glow.in_flight_tally().park_flushed, 1, "{prefix:?}");
        assert_eq!(
            glow.typed_credits_within(quiet),
            0,
            "{prefix:?}: the credit is spent"
        );
        assert_eq!(
            v2_cols(&glow, 11).contains(&1),
            swept,
            "{prefix:?}: the landing cell: {:?}",
            v2_cols(&glow, 11)
        );
        // The late rewrite: refused (the designed verdict), and the
        // landing cell stays as the flush left it.
        row.push('h');
        let ret = quiet + ms(50);
        probe(&mut glow, &row, 6, ret);
        glow.tick(Some((11, 6)), ret, &c, g, &mut out);
        assert_eq!(
            ring_rows(&glow).last(),
            Some(&("no-fresh-hint", "none", (11, 2), (11, 6))),
            "{prefix:?}: {:?}",
            ring_rows(&glow)
        );
        assert_eq!(v2_cols(&glow, 11).contains(&1), swept, "{prefix:?}");
    }
}

/// A DELIVERED INSERT ECHOED THROUGH AN INK PARK IS LAID. Under Ink's
/// park-and-rewrite model the caret parks left, then
/// rewrites the row to its end plus the new text — and a dropped path
/// takes that route too. `park_candidate` asked for a typed stamp or an
/// in-flight credit, and a fresh delivered insert was neither, so the
/// park was judged keyless (refused, fine) and the rewrite observed
/// from the park LANDING: `2 -> 68`, wider than the insert's reach by
/// the prefix's width, refused `no-fresh-hint`, a pending hop no
/// receipt of width 63 could claim — the whole drop dark. With one key
/// in flight the park was held, the rewrite was not a paid return (one
/// press against 64 cells), and the flush lit the prompt cell and spent
/// the key before the same refusal. A fresh delivered insert now holds
/// a park like a stamp does, and the rewrite is a return when the
/// insert's echo shape fits from the park's ORIGIN; the insert arm then
/// lays `origin..target`, paying any surplus from the pre-park presses.
///
/// RED before the fix: `dark=[5..=67] lit=0`, ring `no-fresh-hint
/// (3,5)->(3,2)`, `no-fresh-hint (3,2)->(3,68)`; with the key in flight
/// `stray(3,1)=true flushed=1`. The control: a park held on the insert
/// alone with no return is flushed at its own clock and refused,
/// exactly as an unheld keyless park.
#[test]
fn an_insert_echoed_through_an_ink_park_is_laid() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut out = Vec::new();
    // No key in flight: the drop alone.
    let mut glow = CursorGlow::default();
    three_visible_echoes(&mut glow, t0, &mut out);
    glow.note_insert_delivered(at(700), InsertWidth::Cells(63));
    glow.tick(Some((3, 2)), at(720), &c, g, &mut out);
    assert!(
        glow.held_park.is_some(),
        "the insert holds the park: {:?}",
        ring_rows(&glow)
    );
    glow.tick(Some((3, 68)), at(760), &c, g, &mut out);
    assert_eq!(
        ring_rows(&glow).last(),
        Some(&("licensed", "insert", (3, 5), (3, 68))),
        "{:?}",
        ring_rows(&glow)
    );
    let dark = dark_in(&glow, 3, 5..68);
    assert!(dark.is_empty(), "dark={dark:?}");
    assert_eq!(glow.insert_tally().lit, 1);
    assert_eq!(glow.in_flight_tally().park_returns, 1);
    assert_eq!(glow.admission_tally().declined, 0);
    // One key in flight ahead of the drop.
    let mut glow = CursorGlow::default();
    three_visible_echoes(&mut glow, t0, &mut out);
    glow.note_typed(at(650));
    glow.note_insert_delivered(at(700), InsertWidth::Cells(63));
    glow.tick(Some((3, 2)), at(720), &c, g, &mut out);
    assert!(glow.held_park.is_some());
    glow.tick(Some((3, 69)), at(760), &c, g, &mut out);
    assert_eq!(
        ring_rows(&glow).last(),
        Some(&("licensed", "insert", (3, 5), (3, 69))),
        "{:?}",
        ring_rows(&glow)
    );
    let dark = dark_in(&glow, 3, 5..69);
    let lit = v2_cols(&glow, 3);
    assert!(dark.is_empty(), "dark={dark:?}");
    assert!(!lit.contains(&1), "the prompt cell lit: {lit:?}");
    assert_eq!(glow.in_flight_tally().park_flushed, 0);
    assert_eq!(
        glow.typed_credits_within(at(760)),
        0,
        "the surplus cell paid by the press"
    );
    // CONTROL: held on the insert alone, no return — flushed at its own
    // clock and refused like an unheld keyless park.
    let mut glow = CursorGlow::default();
    three_visible_echoes(&mut glow, t0, &mut out);
    glow.note_insert_delivered(at(700), InsertWidth::Cells(63));
    glow.tick(Some((3, 2)), at(720), &c, g, &mut out);
    glow.tick(Some((3, 2)), at(720) + past_park_window(), &c, g, &mut out);
    assert_eq!(
        ring_rows(&glow).last(),
        Some(&("no-fresh-hint", "none", (3, 5), (3, 2))),
        "{:?}",
        ring_rows(&glow)
    );
    assert_eq!(glow.in_flight_tally().park_flushed, 1);
    assert!(glow.held_park.is_none());
}

/// A LATE PARK FLUSH CANNOT SPEND A STAMP BANKED AFTER THE PARK.
/// `flush_park` judges the park at its own
/// clock, and every freshness read is `now.saturating_duration_since(t)
/// <= window` — true for any `t >= now`. A key typed AFTER a held park
/// (S6c: the stalled key's rewrite held on the pool, sitting until the
/// next tick) was therefore in the bank when the flush ran: the park
/// was licensed as a typed re-anchor on the NEW key's stamp (the prompt
/// cell lit, the stamp popped, a credit spent), the S6c verdict never
/// happened, and the new key's own echo was refused dark. The flush
/// now judges against the banks as they stood at the park: the stamps
/// and presses banked after it are set aside for the judgment and put
/// back after it, so a refusal's forget drops only the presses in
/// flight at the park.
///
/// RED before the fix: `k5 stamp survived flush=false stray(3,1)=true
/// dark=[5, 6]`, ring after the flush `licensed key (3,5)->(3,2)`, then
/// `no-fresh-hint (3,2)->(3,7)`.
#[test]
fn a_late_park_flush_cannot_spend_a_stamp_banked_after_the_park() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    three_visible_echoes(&mut glow, t0, &mut out);
    glow.note_typed(at(400));
    glow.tick(Some((3, 2)), at(700), &c, g, &mut out);
    assert!(
        glow.held_park.is_some(),
        "held on the pool: {:?}",
        ring_rows(&glow)
    );
    let k5 = at(2700);
    glow.note_typed(k5);
    assert_eq!(glow.typed_credits_within(k5), 2);
    // The flush, on the first tick after the new key.
    glow.tick(Some((3, 2)), k5 + Duration::from_millis(5), &c, g, &mut out);
    assert_eq!(
        ring_rows(&glow).last(),
        Some(&("no-fresh-hint", "none", (3, 5), (3, 2))),
        "S6c's verdict at the park's own clock: {:?}",
        ring_rows(&glow)
    );
    assert!(
        glow.type_hint
            .any_fresh(k5 + Duration::from_millis(10), CursorGlow::TYPE_HINT_FRESH),
        "the new key's stamp survives the flush"
    );
    assert_eq!(
        glow.typed_credits_within(k5 + Duration::from_millis(10)),
        1,
        "the refusal forgot only the press in flight at the park"
    );
    assert!(
        !v2_cols(&glow, 3).contains(&1),
        "the prompt cell lit: {:?}",
        v2_cols(&glow, 3)
    );
    // The new key's own echo, from where the rewrite left the caret.
    glow.tick(
        Some((3, 3)),
        k5 + Duration::from_millis(40),
        &c,
        g,
        &mut out,
    );
    assert_eq!(
        ring_rows(&glow).last(),
        Some(&("licensed", "key", (3, 2), (3, 3))),
        "{:?}",
        ring_rows(&glow)
    );
    assert!(v2_cols(&glow, 3).contains(&2));
}

/// A PARK FLUSHED BEFORE A SCROLL OR BAND MOVE LAYS IN THE MOVED ROWS.
/// A key-class edge (`note_return`) flushes a held park — and, flushed
/// before the translation, `note_scroll`
/// flushed it before translating too — but the flush's v2 events (the
/// re-anchor's landing `Sweep`, the `Move`) sit in the engine's buffer
/// in PRE-move rows, and `Engine::translate_scroll` / `translate_band`
/// moved the laid marks and the caret but not the buffered events: the
/// next tick laid the landing sweep one row below the text it belongs
/// to, under whatever scrolled into that row. The buffered
/// row-addressed events now move with the marks: a `Sweep` / `Rewrite`
/// under the mark law (dropped when carried past the band's edge, like
/// a ribbon cell), a `Move`'s ends under the position law (saturated,
/// like the caret). The first shape is now the park RIDING the scroll
/// and flushing past its window on the moved row — the same cells,
/// laid there directly.
///
/// RED before the fix: `row3={1} row2={2, 3, 4}` — the landing cell
/// laid on the row the cohort left.
#[test]
fn a_park_flushed_before_a_scroll_or_band_move_lays_in_the_moved_rows() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut out = Vec::new();
    for shape in [
        "scroll-then-flush",
        "return-then-scroll",
        "return-then-band",
    ] {
        let mut glow = CursorGlow::default();
        three_visible_echoes(&mut glow, t0, &mut out);
        glow.note_typed(at(400));
        glow.tick(Some((3, 2)), at(420), &c, g, &mut out);
        assert!(glow.held_park.is_some(), "{shape}: held");
        let tick_at = match shape {
            "scroll-then-flush" => {
                glow.note_scroll(1);
                assert_eq!(
                    glow.held_park.map(|p| p.row),
                    Some(2),
                    "{shape}: the park rides the scroll"
                );
                at(420) + past_park_window()
            }
            "return-then-scroll" => {
                glow.note_return(at(440));
                assert_eq!(glow.in_flight_tally().park_flushed, 1, "{shape}");
                glow.note_scroll(1);
                at(460)
            }
            _ => {
                glow.note_return(at(440));
                assert_eq!(glow.in_flight_tally().park_flushed, 1, "{shape}");
                glow.note_band_move(0, 3, -1);
                at(460)
            }
        };
        glow.tick(Some((2, 2)), tick_at, &c, g, &mut out);
        assert_eq!(glow.in_flight_tally().park_flushed, 1, "{shape}");
        let row3 = v2_cols(&glow, 3);
        let row2 = v2_cols(&glow, 2);
        assert!(
            row3.is_empty(),
            "{shape}: nothing may be laid on the row the text left: row3={row3:?} row2={row2:?} ring: {:?}",
            ring_rows(&glow)
        );
        assert!(
            row2.contains(&1),
            "{shape}: the flush's landing rides with its row: row2={row2:?}"
        );
        assert_eq!(
            glow.v2.caret_mirror().0,
            2,
            "{shape}: the mirror on the moved row"
        );
    }
}
