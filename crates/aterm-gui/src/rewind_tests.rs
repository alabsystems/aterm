// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rewind: where a view opens and steps, what its band says, what it withholds, and what it draws.

use super::*;
use crate::input::InputEvent;
use crate::scripted_redraw_fixture::{scripted_app, surface};
use crate::temporal::{InputSlice, SpineMark, SpinePre, TemporalMsg};
use crate::term_lock;
use aterm_core::render::RenderInput;
use aterm_core::terminal::{ClockReading, RenderCell};
use aterm_types::keyboard::{Key, KeyEventType, Modifiers};
use aterm_window::event::{ElementState, MouseButton};
use aterm_window::keyboard::{Key as WinitKey, ModifiersState, NamedKey};

const WID: WindowId = WindowId(0);

fn t(us: u64) -> Ticks {
    Ticks(us)
}

fn at(us: &[u64]) -> Vec<RewindInstant> {
    us.iter()
        .map(|&us| RewindInstant {
            at: t(us),
            command: false,
        })
        .collect()
}

#[test]
fn a_view_opens_at_or_before_its_target_and_refuses_before_the_oldest_state() {
    let instants = at(&[100, 200, 300]);
    let now = t(5_000_000);
    assert_eq!(place(&instants, Target::Latest, now), Ok(2));
    assert_eq!(place(&instants, Target::At(t(250)), now), Ok(1));
    assert_eq!(place(&instants, Target::At(t(300)), now), Ok(2));
    assert_eq!(place(&instants, Target::At(t(9_999)), now), Ok(2));
    assert_eq!(
        place(&at(&[4_000_000, 4_600_000]), Target::Ago(1), now),
        Ok(0)
    );
    assert_eq!(
        place(&instants, Target::At(t(99)), now),
        Err("that is before the oldest kept state (4s ago)".to_string())
    );
    assert_eq!(
        place(&[], Target::Latest, now),
        Err("nothing recorded replays yet".to_string())
    );
}

#[test]
fn a_step_moves_by_slices_seconds_or_command_starts_and_stops_at_either_end() {
    const S: u64 = 1_000_000;
    let mut instants = at(&[0, S / 2, S, 26 * S / 10, 27 * S / 10, 6 * S]);
    instants[2].command = true;
    instants[4].command = true;
    let step = |from: u64, by| step_to(&instants, t(from), by);
    assert_eq!(step(6 * S, StepBy::Slice(-1)), Ok(4));
    assert_eq!(
        step(S + 1, StepBy::Slice(1)),
        Ok(3),
        "from the instant at or before"
    );
    assert_eq!(step(0, StepBy::Slice(-5)), Ok(0));
    assert_eq!(step(0, StepBy::Slice(70)), Ok(5));
    assert_eq!(
        step(27 * S / 10, StepBy::Secs(-1)),
        Ok(2),
        "1.7 s: back to 1.0 s"
    );
    assert_eq!(step(S / 2, StepBy::Secs(-1)), Ok(0));
    assert_eq!(step(0, StepBy::Secs(-1)), Ok(0));
    assert_eq!(
        step(S, StepBy::Secs(1)),
        Ok(3),
        "nothing within a second: the next instant"
    );
    assert_eq!(step(26 * S / 10, StepBy::Secs(1)), Ok(4));
    assert_eq!(step(6 * S, StepBy::Secs(1)), Ok(5));
    assert_eq!(step(6 * S, StepBy::Cmd(-1)), Ok(4));
    assert_eq!(step(6 * S, StepBy::Cmd(-2)), Ok(2));
    assert_eq!(step(6 * S, StepBy::Cmd(-3)), Ok(2));
    assert_eq!(step(27 * S / 10, StepBy::Cmd(-1)), Ok(2));
    assert_eq!(
        step(S, StepBy::Cmd(-1)),
        Err("no earlier command start is kept".to_string())
    );
    assert_eq!(step(0, StepBy::Cmd(1)), Ok(2));
    assert_eq!(step(26 * S / 10, StepBy::Cmd(1)), Ok(4));
    assert_eq!(
        step(27 * S / 10, StepBy::Cmd(1)),
        Err("no later command start is recorded".to_string())
    );
    assert_eq!(step(S, StepBy::End(false)), Ok(0));
    assert_eq!(step(S, StepBy::End(true)), Ok(5));
    assert!(step_to(&[], t(0), StepBy::Slice(1)).is_err());
}

/// A view whose shown state the recorder evicted steps Right onto the oldest kept state, and never Left onto a newer one.
#[test]
fn a_step_from_an_evicted_state_goes_right_to_the_oldest_kept_one_and_refuses_left() {
    let mut instants = at(&[100, 200, 300]);
    instants[1].command = true;
    let step = |by| step_to(&instants, t(50), by);
    assert_eq!(step(StepBy::Slice(1)), Ok(0), "Right: the oldest kept");
    assert_eq!(step(StepBy::Slice(2)), Ok(1));
    assert_eq!(
        step(StepBy::Secs(1)),
        Ok(2),
        "a second on from the shown instant"
    );
    assert_eq!(step(StepBy::Cmd(1)), Ok(1));
    assert_eq!(step(StepBy::End(false)), Ok(0));
    assert_eq!(step(StepBy::End(true)), Ok(2));
    for back in [
        StepBy::Slice(-1),
        StepBy::Slice(0),
        StepBy::Secs(-1),
        StepBy::Cmd(-1),
    ] {
        assert_eq!(
            step(back),
            Err(EVICTED.to_string()),
            "{back:?} shows nothing newer than the shown state"
        );
    }
    assert_eq!(
        step_to(&instants, t(100), StepBy::Slice(-1)),
        Ok(0),
        "negative control: from a kept instant Left stops at the oldest"
    );
}

#[test]
fn the_arrows_step_by_slice_second_command_or_end_and_b_esc_and_paging_have_their_uses() {
    let plain = ModifiersState::empty();
    let left = WinitKey::Named(NamedKey::ArrowLeft);
    let right = WinitKey::Named(NamedKey::ArrowRight);
    let named = |k| WinitKey::Named(k);
    let ch = |c: &str| WinitKey::Character(c.into());
    for (key, mods, want) in [
        (&left, plain, KeyUse::Step(StepBy::Slice(-1))),
        (&right, plain, KeyUse::Step(StepBy::Slice(1))),
        (&left, ModifiersState::ALT, KeyUse::Step(StepBy::Secs(-1))),
        (&right, ModifiersState::ALT, KeyUse::Step(StepBy::Secs(1))),
        (&left, ModifiersState::SHIFT, KeyUse::Step(StepBy::Cmd(-1))),
        (&right, ModifiersState::SHIFT, KeyUse::Step(StepBy::Cmd(1))),
        (
            &left,
            ModifiersState::SUPER,
            KeyUse::Step(StepBy::End(false)),
        ),
        (
            &right,
            ModifiersState::SUPER,
            KeyUse::Step(StepBy::End(true)),
        ),
        (
            &named(NamedKey::Home),
            plain,
            KeyUse::Step(StepBy::End(false)),
        ),
        (
            &named(NamedKey::End),
            plain,
            KeyUse::Step(StepBy::End(true)),
        ),
        (&named(NamedKey::PageUp), plain, KeyUse::Page(1)),
        (&named(NamedKey::PageDown), plain, KeyUse::Page(-1)),
        (&named(NamedKey::Escape), plain, KeyUse::Close),
        (&ch("b"), plain, KeyUse::Changed),
        (&ch("B"), ModifiersState::SHIFT, KeyUse::Changed),
        (&ch("a"), plain, KeyUse::Withhold),
        (&ch("b"), ModifiersState::CONTROL, KeyUse::Withhold),
        (
            &left,
            ModifiersState::SHIFT | ModifiersState::ALT,
            KeyUse::Withhold,
        ),
        (&ch("c"), ModifiersState::SUPER, KeyUse::Pass),
    ] {
        assert_eq!(key_use(key, mods, false), want, "{key:?} {mods:?}");
    }
    assert_eq!(
        key_use(&ch("a"), plain, true),
        KeyUse::Pass,
        "a modifier press"
    );
}

#[test]
fn a_withheld_press_text_paste_click_or_wheel_is_noticed_and_a_release_or_move_is_not() {
    let key = |event_type| InputEvent::Key {
        key: Key::Character('x'),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type,
    };
    let shift = InputEvent::Key {
        key: Key::Named(aterm_types::keyboard::NamedKey::ShiftLeft),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    };
    let px_off = crate::input::PixelOffset::CELL_ORIGIN;
    let side = aterm_core::selection::SelectionSide::Left;
    let click = |pressed| InputEvent::MouseButton {
        button: aterm_types::mouse::MouseButton::Left,
        pressed,
        row: 1,
        col: 1,
        mods: 0,
        click_count: 1,
        side,
        block: false,
        suppress_copy_on_select: false,
        px_off,
    };
    let wheel = InputEvent::Wheel {
        dir: aterm_types::mouse::WheelDir::Up,
        lines: 1,
        detent: true,
        row: 1,
        col: 1,
        mods: 0,
        px_off,
    };
    let hover = InputEvent::MouseMove {
        buttons: 0,
        row: 1,
        col: 1,
        mods: 0,
        side,
        px_off,
    };
    for (ev, noticed_too) in [
        (key(KeyEventType::Press), true),
        (key(KeyEventType::Repeat), true),
        (key(KeyEventType::Release), false),
        (shift, false),
        (InputEvent::Text("x".into()), true),
        (InputEvent::KeySequence(b"x".to_vec()), true),
        (
            InputEvent::Paste("x".into(), crate::input::PasteFraming::AtDrain),
            true,
        ),
        (click(true), true),
        (click(false), false),
        (wheel, true),
        (hover, false),
    ] {
        assert!(withheld(&ev), "{ev:?}");
        assert_eq!(noticed(&ev), noticed_too, "{ev:?}");
    }
    assert!(!withheld(&InputEvent::Focus(true)));
    assert!(!noticed(&InputEvent::Focus(true)));
}

#[test]
fn a_changed_cell_note_escapes_every_control_byte_and_says_where_and_when() {
    assert_eq!(
        escaped(b"a\x1b[1;1Hz\r\n\t\\\x07\xff", 64),
        "a\\e[1;1Hz\\r\\n\\t\\\\\\x07\\xff"
    );
    assert_eq!(escaped(b"abcdef", 4), "abc\u{2026}");
    let found = |at, changed, bytes: &[u8]| Found {
        session: 0,
        token: NO_TOKEN,
        cell: (17, 41),
        at,
        changed,
        bytes: bytes.to_vec(),
        resize: None,
    };
    let now = t(13_400_000);
    assert_eq!(
        changed_note(&found(Some(t(1_000_000)), true, b"\x1b[18;42Hx"), now),
        "row 18 col 42 changed 12.4 s ago: 9 bytes \"\\e[18;42Hx\""
    );
    let mut resized = found(Some(t(1_000_000)), true, b"");
    resized.resize = Some((24, 80));
    assert_eq!(
        changed_note(&resized, now),
        "row 18 col 42 changed 12.4 s ago: 0 bytes, resize to 80x24 \"\""
    );
    assert_eq!(
        changed_note(&found(Some(t(1_000_000)), false, b""), now),
        "row 18 col 42 is as at the oldest kept state, 12.4 s ago"
    );
    assert_eq!(
        changed_note(&found(None, false, b""), now),
        "row 18 col 42: nothing recorded replays"
    );
}

fn view_at(state: (usize, usize), faith: Faith) -> RewindView {
    let mut view = RewindView::new(1_000);
    view.shown = Some(Arc::new(Mutex::new(Terminal::new(4, 20))));
    view.at = t(1_200);
    view.state = state;
    view.faith = faith;
    view.shown_unix_ms = 1_000_000;
    view
}

#[test]
fn the_band_and_status_say_unchecked_until_a_witness_judges_and_faithful_only_on_a_match() {
    assert_eq!(Faith::of(Fidelity::Unwitnessed), Faith::Unchecked);
    assert_eq!(Faith::of(Fidelity::Diverged), Faith::MayDiffer);
    assert_eq!(Faith::of(Fidelity::Faithful), Faith::Faithful);
    let view = view_at((803, 1412), Faith::Unchecked);
    assert_eq!(
        band_text(&view, 1_042_000, 3_600, 1_000, None),
        "REWIND  unchecked  01:16:40 (-0:42)  state 803/1412  Left/Right step  Esc live"
    );
    assert_eq!(
        band_text(&view, 1_042_000, 3_600, 1_000 + 3_000, None),
        "REWIND  unchecked  01:16:40 (-0:42)  state 803/1412  live +3 KB  Left/Right step  Esc \
         live"
    );
    let mut noted = view_at((803, 1412), Faith::Unchecked);
    noted.note = Some("no earlier command start is kept".into());
    assert_eq!(
        band_text(&noted, 1_042_000, 3_600, 1_000, Some("effects paused")),
        "REWIND  unchecked  01:16:40 (-0:42)  state 803/1412  effects paused  no earlier command \
         start is kept"
    );
    assert_eq!(
        status_line(true, Some(&view), 1_500),
        "recording=true view=open at=1200 state=803/1412 faithful=unchecked live_bytes=500"
    );
    let differs = view_at((1, 2), Faith::MayDiffer);
    assert!(
        band_text(&differs, 1_000_000, 0, 0, None)
            .contains("  may differ from what was on screen  "),
        "{}",
        band_text(&differs, 1_000_000, 0, 0, None)
    );
    assert!(status_line(true, Some(&differs), 0).contains(" faithful=no "));
    assert!(!band_text(&differs, 1_000_000, 0, 0, None).contains("faithful"));
    assert_eq!(
        status_line(false, None, 7),
        "recording=false view=closed at=- state=- faithful=- live_bytes=0"
    );
    assert_eq!(ago(3_725), "-1:02:05");
    assert_eq!(clock(-1_000, 0), "23:59:59");
}

/// Start recording session 0 from its screen now: a keyframe of the live engine seeded on the spine.
pub(crate) fn start_recording(app: &App) {
    let s = app.pool.get(0).expect("session 0");
    let (checkpoint, pre) = {
        let term = term_lock(s.live_term());
        (term.checkpoint(), SpinePre::take(&term))
    };
    let mut rec = s.ctx.temporal.lock().expect("recorder");
    rec.record_keyframe(checkpoint);
    rec.spine_watermark().seed(pre);
}

/// Feed `bytes` to session 0's engine and, as its reader does, through a spine mark into its recorder, as one slice at a later instant than any before it; that instant.
pub(crate) fn slice(app: &App, bytes: &[u8]) -> Ticks {
    let s = app.pool.get(0).expect("session 0");
    let mut rec = s.ctx.temporal.lock().expect("recorder");
    let last = rec.latest_tick();
    while rec.now() <= last {}
    let (tx, rx) = crate::temporal::queue(8);
    let mut mark = SpineMark::attach(rec.spine_watermark());
    {
        let mut term = term_lock(s.live_term());
        let pre = SpinePre::take(&term);
        let clock = ClockReading::now();
        term.process_at(bytes, clock);
        mark.record(&tx, pre, &term, InputSlice::at(bytes, clock));
    }
    while let Some(msg) = rx.try_recv() {
        rec.apply_inline(msg);
    }
    rec.latest_tick()
}

/// Record a live witness of `term`'s screen on session 0's spine, at a later instant than any before it, as the reader does.
fn witness(app: &App, term: &Terminal) {
    let s = app.pool.get(0).expect("session 0");
    let mut rec = s.ctx.temporal.lock().expect("recorder");
    let last = rec.latest_tick();
    while rec.now() <= last {}
    let live = term.checkpoint_visible().expect("a visible checkpoint");
    rec.apply_inline(TemporalMsg::Witness(Arc::new(live)));
}

fn replay(app: &App, at: Option<Ticks>) -> Terminal {
    let s = app.pool.get(0).expect("session 0");
    let plan = s
        .ctx
        .temporal
        .lock()
        .expect("recorder")
        .replay_plan(at)
        .expect("a replay");
    crate::rewind_worker::show_inline(crate::rewind_worker::Show {
        session: 0,
        token: NO_TOKEN,
        plan,
        judge: None,
        colors: None,
    })
    .term
}

fn instants(app: &App) -> Vec<RewindInstant> {
    let s = app.pool.get(0).expect("session 0");
    s.ctx.temporal.lock().expect("recorder").rewind_instants()
}

fn view(app: &App) -> &RewindView {
    app.pool.rewind(0).expect("an open view")
}

fn shown_text(app: &App) -> String {
    let term = view(app).shown.clone().expect("a shown fold");
    crate::control::screen_text(&term_lock(&term))
}

fn live_text(app: &App) -> String {
    let live = app.pool.get(0).expect("session 0").live_term().clone();
    crate::control::screen_text(&term_lock(&live))
}

/// A token no view's fold carries, for folds the tests make by hand.
const NO_TOKEN: FoldToken = FoldToken { view: 0, serial: 0 };

fn grid(app: &App) -> (usize, usize) {
    let ws = &app.windows[&WID];
    (usize::from(ws.rows), usize::from(ws.cols))
}

fn cell_frame(term: &mut Terminal, (rows, cols): (usize, usize)) -> Vec<Vec<RenderCell>> {
    let mut frame = RenderInput::default();
    term.cell_frame_into(&mut frame, rows, cols);
    frame.cells.iter().map(|row| clip(row, cols)).collect()
}

/// A frame row's cells within `cols`; a row holds only the cells its line has.
fn clip(row: &[RenderCell], cols: usize) -> Vec<RenderCell> {
    row[..row.len().min(cols)].to_vec()
}

/// The window's last frame, its grid rows only.
fn drawn(app: &App) -> Vec<Vec<RenderCell>> {
    let (rows, cols) = grid(app);
    let cells = &app.windows[&WID].input_scratch.cells;
    let strip = cells.len() - rows;
    cells[strip..].iter().map(|row| clip(row, cols)).collect()
}

fn text(rows: &[Vec<RenderCell>]) -> String {
    rows.iter()
        .map(|row| {
            row.iter()
                .map(|c| c.ch)
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A pressed key event shaped like the platform's, for [`App::on_key`].
fn character(ch: &str) -> aterm_window::event::KeyEvent {
    use aterm_window::keyboard::{KeyCode, KeyLocation, PhysicalKey, SmolStr};
    aterm_window::event::KeyEvent::synthetic_for_test(
        PhysicalKey::Code(KeyCode::KeyA),
        WinitKey::Character(SmolStr::new(ch)),
        Some(SmolStr::new(ch)),
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    )
}

/// The platform's "open link" modifier.
fn link_modifier() -> ModifiersState {
    if cfg!(target_os = "macos") {
        ModifiersState::SUPER
    } else {
        ModifiersState::CONTROL
    }
}

fn esc() -> WinitKey {
    WinitKey::Named(NamedKey::Escape)
}

fn key(app: &mut App, mods: ModifiersState, key: &WinitKey) -> bool {
    app.rewind_key(WID, mods, key, false)
}

fn arrow(left: bool) -> WinitKey {
    WinitKey::Named(if left {
        NamedKey::ArrowLeft
    } else {
        NamedKey::ArrowRight
    })
}

/// Put the pointer over the middle of grid cell `(row, col)` of the window, through the real handler.
fn hover(app: &mut App, row: u16, col: u16) {
    hover_across(app, row, col, 0.5);
}

/// Put the pointer `across` (0 to 1) the width of grid cell `(row, col)`.
fn hover_across(app: &mut App, row: u16, col: u16, across: f64) {
    let (cw, ch) = app.win_cell_size(WID);
    let strip_px = f64::from(app.tab_strip_rows) * ch as f64;
    let x = app.win_pad(WID) as f64 + cw as f64 * (f64::from(col) + across);
    let y = (app.win_pad_top(WID) + app.win_head(WID)) as f64
        + strip_px
        + ch as f64 * (f64::from(row) + 0.5);
    app.on_cursor_moved(WID, x, y);
}

/// R0 exit item 4: a rewound pane draws exactly the replayed engine's cells under its track and band, and Esc draws the live engine's again.
#[test]
fn a_rewound_pane_draws_the_replayed_screen_and_esc_draws_live_again() {
    let mut app = scripted_app();
    app.redraw_window(WID);
    start_recording(&app);
    let past = slice(&app, b"\x1b[2J\x1b[Hthe past screen\r\nits second row");
    let _ = slice(&app, b"\x1b[2J\x1b[Hthe live screen now");
    let dims = grid(&app);
    let want_past = cell_frame(&mut replay(&app, Some(past)), dims);
    let want_live = cell_frame(&mut replay(&app, None), dims);
    assert!(
        text(&want_live).starts_with("the live screen now"),
        "fixture: the replay's newest state is the live screen: {}",
        text(&want_live)
    );
    app.redraw_window(WID);
    assert_eq!(text(&drawn(&app)), text(&want_live), "fixture: live before");

    app.rewind_open(0, Target::At(past), None).expect("open");
    let commits = surface(&mut app).commits();
    app.redraw_window(WID);
    assert!(
        surface(&mut app).commits() > commits,
        "the rewound frame presents"
    );
    let shown = drawn(&app);
    let (rows, _) = dims;
    assert_eq!(
        &shown[..rows - 2],
        &want_past[..rows - 2],
        "the replayed cells"
    );
    assert_ne!(
        text(&shown[..rows - 2]),
        text(&want_live[..rows - 2]),
        "negative control: the rewound frame is not the live screen"
    );
    let band = text(&shown[rows - 1..]);
    assert!(
        band.starts_with(" REWIND  unchecked  ") && band.contains("  state 2/3  "),
        "the band: {band:?}"
    );
    assert!(
        !band.contains("faithful"),
        "no witness, no faithful: {band:?}"
    );
    let track = text(&shown[rows - 2..rows - 1]);
    assert!(
        track.contains(crate::rewind_band::SHOWN) && track.contains(crate::rewind_band::KEPT),
        "the track over the band: {track:?}"
    );
    assert!(
        !track.contains(crate::rewind_band::HATCH),
        "nothing lost or mismatched: {track:?}"
    );
    let border = app.windows[&WID].rewind_quads.clone();
    assert_eq!(
        border.len(),
        2 + 2 * rows,
        "the border: a top and a bottom stroke, and a left and a right one cut per cell row"
    );
    assert!(
        border
            .iter()
            .all(|q| app.windows[&WID].input_scratch.nova_add.contains(q)),
        "the border is in the frame's quads"
    );
    assert!(
        live_text(&app).starts_with("the live screen now"),
        "the live engine is untouched"
    );

    assert!(app.rewind_key(WID, ModifiersState::empty(), &esc(), false));
    assert!(!app.pool.is_rewound(0));
    app.redraw_window(WID);
    assert_eq!(
        drawn(&app),
        want_live,
        "Esc draws the live screen, band and all gone"
    );
    assert!(
        app.windows[&WID].rewind_quads.is_empty(),
        "the border is taken back"
    );
    assert!(
        !border
            .iter()
            .any(|q| app.windows[&WID].input_scratch.nova_add.contains(q)),
        "the frame carries none of the border"
    );
}

/// D13 on a capture (the `image` verb, a recording): a rewound pane reads as not live and no capture presents the pet over it, and once live one does again.
#[test]
fn a_capture_reads_a_rewound_pane_as_not_live_and_presents_no_pet_over_it() {
    use crate::app_render::ComposedCursorFxClock;
    use std::time::{Duration, Instant};
    let mut app = App::headless_for_test();
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty pet".into());
    app.config.motion = Some("full".into());
    app.windows.get_mut(&WID).expect("window").focused = true;
    app.recompute_sparkle();
    assert!(app.trail_is_kitty_pet(), "fixture: the pet is the trail");
    let t0 = Instant::now();
    let at = |k: u64| t0 + Duration::from_millis(16 * k);
    let presented = |app: &mut App, frames: std::ops::Range<u64>| {
        frames.into_iter().any(|k| {
            app.capture_words(WID, at(k));
            app.windows[&WID].fx.companion().hit_rect().is_some()
        })
    };
    let live = |app: &mut App, k: u64| {
        app.prepare_terminal_capture_grid_with_cursor_fx(WID, ComposedCursorFxClock::Advance(at(k)))
            .and_then(|grid| grid.focus)
            .map(|focus| focus.live_viewport)
    };
    assert!(
        presented(&mut app, 0..600),
        "fixture: a live capture presents the pet"
    );
    assert_eq!(
        live(&mut app, 600),
        Some(true),
        "fixture: a live capture is live"
    );
    start_recording(&app);
    let past = slice(&app, b"\x1b[2J\x1b[Hthe past screen");
    let _ = slice(&app, b"\x1b[2J\x1b[Hthe live screen now");

    app.rewind_open(0, Target::At(past), None).expect("open");
    assert_eq!(
        live(&mut app, 601),
        Some(false),
        "a rewound pane is not live"
    );
    assert!(
        !presented(&mut app, 602..662),
        "no capture presents the pet over a rewound pane"
    );

    assert!(app.rewind_key(WID, ModifiersState::empty(), &esc(), false));
    assert_eq!(live(&mut app, 662), Some(true), "live again");
    assert!(
        presented(&mut app, 663..1263),
        "negative control: once live, a capture presents the pet again"
    );
}

/// The fixture's session writing to a socket the test reads, as its program would.
#[cfg(unix)]
fn piped() -> (App, std::os::unix::net::UnixStream) {
    use crate::scripted_redraw_fixture::scripted_app_with_sink;

    let (reader, writer) = std::os::unix::net::UnixStream::pair().expect("an input observer");
    reader.set_nonblocking(true).expect("nonblocking");
    let sink = Arc::new(aterm_session::sink::SinkWriter::new_owned(writer.into()));
    (scripted_app_with_sink(sink), reader)
}

#[cfg(unix)]
fn program_read(reader: &mut std::os::unix::net::UnixStream) -> Vec<u8> {
    use std::io::Read;
    let mut bytes = [0; 256];
    match reader.read(&mut bytes) {
        Ok(n) => bytes[..n].to_vec(),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Vec::new(),
        Err(e) => panic!("reading the program's input: {e}"),
    }
}

#[cfg(unix)]
fn press(c: char) -> InputEvent {
    InputEvent::Key {
        key: Key::Character(c),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    }
}

#[cfg(unix)]
#[test]
fn a_persons_keys_reach_no_program_while_rewound_and_reach_it_again_once_live() {
    use crate::input::{InputOutcome, Source};

    let (mut app, mut program) = piped();
    start_recording(&app);
    let _ = slice(&app, b"$ ");
    assert_eq!(app.input(WID, press('a'), Source::Human), InputOutcome::Ok);
    assert_eq!(
        program_read(&mut program),
        b"a",
        "fixture: a key reaches the program"
    );
    app.rewind_open(0, Target::Latest, None).expect("open");
    for ev in [
        press('b'),
        InputEvent::Text("cd".into()),
        InputEvent::Paste("ef".into(), crate::input::PasteFraming::AtDrain),
    ] {
        assert_eq!(app.input(WID, ev, Source::Human), InputOutcome::Ok);
    }
    app.deliver_paste(WID, None, "gh\nij".into(), Source::Human);
    assert!(
        program_read(&mut program).is_empty(),
        "nothing reached the program"
    );
    let notice = app
        .messages
        .live_by_key(&read_only_key(0))
        .expect("the read-only notice");
    assert!(
        notice.msg.title.starts_with("Rewind is showing "),
        "{:?}",
        notice.msg.title
    );
    assert_eq!(
        app.input(WID, press('k'), Source::Controller),
        InputOutcome::Ok
    );
    assert_eq!(
        program_read(&mut program),
        b"k",
        "a client's key still reaches the program (as built)"
    );
    assert!(app.rewind_close(0));
    assert!(
        app.messages.live_by_key(&read_only_key(0)).is_none(),
        "the notice goes with the view"
    );
    assert_eq!(app.input(WID, press('z'), Source::Human), InputOutcome::Ok);
    assert_eq!(program_read(&mut program), b"z", "live again");
}

fn ask(app: &mut App, asked: crate::rewind_verb::Asked) -> Result<String, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    app.control_rewind(0, asked, tx);
    rx.try_recv().expect("an inline fold answers at once")
}

#[test]
fn the_verb_opens_steps_reads_and_closes_a_view_that_claims_no_faithfulness_unwitnessed() {
    use crate::rewind_verb::Asked;
    let mut app = scripted_app();
    assert_eq!(
        ask(&mut app, Asked::Open(Target::Latest)),
        Err(NOT_RECORDING.to_string()),
        "a session that keeps no history"
    );
    assert!(!app.pool.is_rewound(0), "no view is left behind a refusal");
    start_recording(&app);
    let first = slice(&app, b"one");
    let _ = slice(&app, b"\r\ntwo");
    let _ = slice(&app, b"\r\nthree");
    let open = ask(&mut app, Asked::Open(Target::At(first))).expect("open");
    assert_eq!(
        open,
        format!(
            "recording=true view=open at={} state=2/4 faithful=unchecked live_bytes=0",
            first.0
        )
    );
    let shown = ask(&mut app, Asked::Text { trim: true }).expect("text");
    let (header, body) = shown.split_once('\n').expect("a header and a body");
    assert_eq!(body, "one\n", "{shown:?}");
    assert!(
        header.starts_with(&format!("OK {} trimmed=", body.len())),
        "{header}"
    );
    let stepped = ask(&mut app, Asked::Step(StepBy::Slice(1))).expect("step");
    assert!(stepped.contains(" state=3/4 "), "{stepped}");
    let shown = ask(&mut app, Asked::Text { trim: true }).expect("text");
    assert!(shown.ends_with("\none\ntwo\n"), "{shown:?}");
    assert!(
        live_text(&app).contains("three"),
        "negative control: the live screen moved on"
    );
    assert!(
        ask(&mut app, Asked::Step(StepBy::Slice(-9)))
            .expect("step")
            .contains(" state=1/4 ")
    );
    assert!(
        ask(&mut app, Asked::Step(StepBy::End(true)))
            .expect("step")
            .contains(" state=4/4 ")
    );
    assert!(
        ask(&mut app, Asked::Status)
            .expect("status")
            .contains(" faithful=unchecked ")
    );
    assert_eq!(
        ask(&mut app, Asked::Close).expect("close"),
        "recording=true view=closed at=- state=- faithful=- live_bytes=0"
    );
    assert_eq!(
        ask(&mut app, Asked::Text { trim: false }),
        Err(NO_VIEW.to_string())
    );
    assert_eq!(
        ask(&mut app, Asked::Step(StepBy::Slice(1))),
        Err(NO_VIEW.to_string())
    );
    assert_eq!(
        ask(&mut app, Asked::Changed(0, 0)),
        Err(NO_VIEW.to_string())
    );
}

/// The view says faithful only where a live witness covering the shown state matched its replay, and may differ from a mismatch on; the track hatches the mismatched stretch once a fold has judged it.
#[test]
fn a_view_is_faithful_where_a_live_witness_matched_and_may_differ_from_a_mismatch_on() {
    use crate::rewind_verb::Asked;
    let mut app = scripted_app();
    start_recording(&app);
    let zero = slice(&app, b"zero");
    let one = slice(&app, b"\r\none");
    let live = app.pool.get(0).expect("session 0").live_term().clone();
    witness(&app, &term_lock(&live));
    let two = slice(&app, b"\r\ntwo");
    let mut stranger = Terminal::new(24, 80);
    stranger.process(b"not what was on screen");
    witness(&app, &stranger);
    let three = slice(&app, b"\r\nthree");
    let fidelity = |app: &App, at| {
        let s = app.pool.get(0).expect("session 0");
        s.ctx.temporal.lock().expect("recorder").fidelity_at(at)
    };
    assert_eq!(
        fidelity(&app, one),
        Fidelity::Unwitnessed,
        "fixture: no fold has judged a witness yet"
    );

    let open = ask(&mut app, Asked::Open(Target::At(one))).expect("open");
    assert!(open.contains(" state=3/5 faithful=yes "), "{open}");
    assert_eq!(
        fidelity(&app, zero),
        Fidelity::Faithful,
        "the witness after a state covers it"
    );
    app.redraw_window(WID);
    let (rows, _) = grid(&app);
    let band = text(&drawn(&app)[rows - 1..]);
    assert!(band.contains("  faithful  "), "{band:?}");
    let track = text(&drawn(&app)[rows - 2..rows - 1]);
    assert!(
        !track.contains(crate::rewind_band::HATCH),
        "negative control: no mismatch judged yet, no hatch: {track:?}"
    );

    let stepped = ask(&mut app, Asked::Step(StepBy::Slice(1))).expect("step");
    assert!(stepped.contains(&format!(" at={} ", two.0)), "{stepped}");
    assert!(stepped.contains(" faithful=no "), "{stepped}");
    app.redraw_window(WID);
    let band = text(&drawn(&app)[rows - 1..]);
    assert!(
        band.contains("  may differ from what was on screen  ") && !band.contains("faithful"),
        "{band:?}"
    );
    let track = text(&drawn(&app)[rows - 2..rows - 1]);
    assert!(
        track.contains(crate::rewind_band::HATCH),
        "the mismatched stretch is hatched: {track:?}"
    );
    assert!(
        view(&app)
            .track
            .spans
            .iter()
            .any(|(_, _, kind)| *kind == crate::temporal::Span::Differs),
        "{:?}",
        view(&app).track
    );
    let stepped = ask(&mut app, Asked::Step(StepBy::Slice(1))).expect("step");
    assert!(stepped.contains(&format!(" at={} ", three.0)), "{stepped}");
    assert!(
        stepped.contains(" faithful=no "),
        "a state after a mismatch may differ too: {stepped}"
    );
    let back = ask(&mut app, Asked::Step(StepBy::Slice(-2))).expect("step");
    assert!(back.contains(" faithful=yes "), "{back}");
}

#[test]
fn the_rewind_instants_are_the_oldest_keyframe_then_each_later_slice() {
    let app = scripted_app();
    assert!(instants(&app).is_empty());
    start_recording(&app);
    let a = slice(&app, b"a");
    let b = slice(&app, b"b");
    let instants = instants(&app);
    assert_eq!(instants.len(), 3, "{instants:?}");
    assert_eq!(
        instants[1..].iter().map(|i| i.at).collect::<Vec<_>>(),
        [a, b]
    );
    assert!(instants[0].at < a);
    assert!(instants.iter().all(|i| !i.command), "no command started");
}

/// A shell's first prompt, marked as OSC 133 marks it.
const FIRST_PROMPT: &[u8] = b"\x1b]133;A\x07$ \x1b]133;B\x07";

/// A command's end and the prompt after it.
const PROMPT: &[u8] = b"\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07";

/// Shift-Left and Shift-Right go to the instants a command started (an OSC 133 `C`), and say so on the band past either end.
#[test]
fn shift_arrows_go_to_the_previous_or_next_command_start_and_refuse_past_either_end() {
    let mut app = scripted_app();
    start_recording(&app);
    let _ = slice(&app, FIRST_PROMPT);
    let _ = slice(&app, b"ls\r\n");
    let ls = slice(&app, b"\x1b]133;C\x07file1\r\n");
    let _ = slice(&app, b"file2\r\n");
    let _ = slice(&app, PROMPT);
    let typed = slice(&app, b"make\r\n");
    let make = slice(&app, b"\x1b]133;C\x07building\r\n");
    let _ = slice(&app, b"done\r\n");
    let starts: Vec<Ticks> = instants(&app)
        .iter()
        .filter(|i| i.command)
        .map(|i| i.at)
        .collect();
    assert_eq!(
        starts,
        [ls, make],
        "fixture: the live engine took both C marks"
    );

    app.rewind_open(0, Target::Latest, None).expect("open");
    let shift = ModifiersState::SHIFT;
    assert!(key(&mut app, shift, &arrow(true)));
    assert_eq!(view(&app).at, make);
    assert!(shown_text(&app).contains("building") && !shown_text(&app).contains("done"));
    assert!(key(&mut app, shift, &arrow(true)));
    assert_eq!(view(&app).at, ls);
    assert!(key(&mut app, shift, &arrow(true)));
    assert_eq!(view(&app).at, ls, "no earlier command start: it stays");
    assert_eq!(
        view(&app).note.as_deref(),
        Some("no earlier command start is kept")
    );
    assert!(key(&mut app, shift, &arrow(false)));
    assert_eq!(view(&app).at, make);
    assert_eq!(view(&app).note, None, "a step that lands clears the note");
    assert!(key(&mut app, shift, &arrow(false)));
    assert_eq!(view(&app).at, make);
    assert_eq!(
        view(&app).note.as_deref(),
        Some("no later command start is recorded")
    );

    assert!(key(&mut app, ModifiersState::empty(), &arrow(true)));
    assert_eq!(
        view(&app).at,
        typed,
        "negative control: a plain step lands on the slice before, not a command start"
    );
    assert_eq!(
        ask(&mut app, crate::rewind_verb::Asked::Step(StepBy::Cmd(-1)))
            .expect("step by=cmd")
            .split(' ')
            .find(|w| w.starts_with("at=")),
        Some(format!("at={}", ls.0).as_str())
    );
}

#[test]
fn a_command_step_refuses_output_that_marks_no_command() {
    let mut app = scripted_app();
    start_recording(&app);
    let _ = slice(&app, b"$ ls\r\n");
    let _ = slice(&app, b"file1\r\n");
    app.rewind_open(0, Target::Latest, None).expect("open");
    let latest = view(&app).at;
    assert_eq!(
        ask(&mut app, crate::rewind_verb::Asked::Step(StepBy::Cmd(-1))),
        Err("no earlier command start is kept".to_string())
    );
    assert_eq!(view(&app).at, latest);
    assert!(
        ask(&mut app, crate::rewind_verb::Asked::Step(StepBy::Slice(-1)))
            .expect("negative control: a plain step goes back")
            .contains(" state=2/3 ")
    );
}

/// A step never shows an instant the live screen showed inside an open mode-2026 frame: it lands on the frame's close.
#[test]
fn a_step_skips_the_instants_inside_an_open_synchronized_frame() {
    let mut app = scripted_app();
    start_recording(&app);
    let before = slice(&app, b"before");
    let half = slice(&app, b"\x1b[?2026h\x1b[2J\x1b[Hhalf");
    let closed = slice(&app, b" drawn\x1b[?2026l");
    let after = slice(&app, b"\r\nafter");
    let kept: Vec<Ticks> = instants(&app).iter().map(|i| i.at).collect();
    assert_eq!(&kept[1..], &[before, closed, after], "{kept:?}");
    let torn = crate::control::screen_text(&replay(&app, Some(half)));
    assert!(
        torn.starts_with("half") && !torn.contains("drawn") && !torn.contains("before"),
        "negative control: the skipped instant replays a torn frame: {torn:?}"
    );

    app.rewind_open(0, Target::Latest, None).expect("open");
    assert!(key(&mut app, ModifiersState::empty(), &arrow(true)));
    assert_eq!(view(&app).at, closed);
    assert!(shown_text(&app).starts_with("half drawn"));
    assert!(key(&mut app, ModifiersState::empty(), &arrow(true)));
    assert_eq!(view(&app).at, before, "over the open frame");
    assert!(shown_text(&app).starts_with("before"));
    app.rewind_open(0, Target::At(half), None).expect("open");
    assert_eq!(
        view(&app).at,
        before,
        "an open at an instant inside the frame lands before it"
    );
}

/// Option-arrows step a second, landing at least one instant away.
#[test]
fn option_arrows_step_a_second_at_a_time() {
    let mut app = scripted_app();
    start_recording(&app);
    let a = slice(&app, b"a");
    let b = slice(&app, b"b");
    let c = slice(&app, b"c");
    app.rewind_open(0, Target::Latest, None).expect("open");
    assert_eq!(view(&app).at, c);
    assert!(key(&mut app, ModifiersState::ALT, &arrow(true)));
    let oldest = instants(&app)[0].at;
    assert_eq!(
        view(&app).at,
        oldest,
        "a second back from slices microseconds apart is before all of them"
    );
    assert!(key(&mut app, ModifiersState::ALT, &arrow(false)));
    assert_eq!(view(&app).at, c, "and a second on is past all of them");
    assert!(key(&mut app, ModifiersState::SUPER, &arrow(true)));
    assert_eq!(view(&app).at, oldest, "Cmd-Left: the oldest");
    assert!(key(&mut app, ModifiersState::empty(), &arrow(false)));
    assert_eq!(
        view(&app).at,
        a,
        "negative control: a plain step goes one slice"
    );
    assert!(key(&mut app, ModifiersState::SUPER, &arrow(false)));
    assert_eq!(view(&app).at, c, "Cmd-Right: the newest");
    let _ = b;
}

/// The past screen keeps its scrollback: the wheel and Page Up scroll it, at most [`PAST_SCROLLBACK`] lines, and the live screen does not move.
#[test]
fn the_past_screen_scrolls_its_own_scrollback_up_to_its_cap() {
    let mut app = scripted_app();
    start_recording(&app);
    let lines: String = (0..3000).map(|n| format!("line {n}\r\n")).collect();
    let past = slice(&app, lines.as_bytes());
    let _ = slice(&app, b"\x1b[2J\x1b[Hthe live screen");
    app.rewind_open(0, Target::At(past), None).expect("open");
    let shown = view(&app).shown.clone().expect("shown");
    let rows = usize::from(term_lock(&shown).rows());
    assert!(
        term_lock(&shown).grid().scrollback_lines() >= PAST_SCROLLBACK,
        "the fold keeps the past's scrollback"
    );
    app.on_mouse_wheel(
        WID,
        aterm_window::event::MouseScrollDelta::LineDelta(0.0, 1.0),
    );
    assert_eq!(view(&app).scrolled, 3, "a wheel notch scrolls three lines");
    let live = app.pool.get(0).expect("session 0").live_term().clone();
    assert_eq!(
        term_lock(&live).grid().display_offset(),
        0,
        "negative control: the live screen is not scrolled"
    );
    assert!(key(
        &mut app,
        ModifiersState::empty(),
        &WinitKey::Named(NamedKey::PageUp)
    ));
    assert!(view(&app).scrolled > 3, "Page Up scrolls a page");
    app.rewind_scroll(0, 100_000);
    assert_eq!(view(&app).scrolled, PAST_SCROLLBACK, "capped");
    app.redraw_window(WID);
    let top = text(&drawn(&app)[..1]);
    assert_eq!(
        top,
        format!("line {}", 3000 - (rows - 1) - PAST_SCROLLBACK),
        "the top row is the past's own scrollback"
    );
    assert!(key(&mut app, ModifiersState::empty(), &arrow(true)));
    assert_eq!(
        view(&app).scrolled,
        0,
        "a step shows the new instant from its bottom"
    );
}

/// A fold from the fork cache shows what the straight fold from the base does, at the cache's own end and between its forks.
#[test]
fn a_fold_from_the_fork_cache_shows_what_the_fold_from_the_base_does() {
    use crate::rewind_worker::{FORK_EVERY, Show, show_inline, warmed_show};
    let app = scripted_app();
    start_recording(&app);
    let (mut ticks, mut total, mut widest) = (Vec::new(), 0, 0);
    for chunk in 0..60 {
        let lines: String = (0..50)
            .map(|n| {
                format!(
                    "\x1b[3{}mchunk {chunk:02} line {n:02} {}\r\n",
                    n % 7,
                    "x".repeat(60)
                )
            })
            .collect();
        (total, widest) = (total + lines.len(), widest.max(lines.len()));
        ticks.push(slice(&app, lines.as_bytes()));
    }
    let s = app.pool.get(0).expect("session 0");
    let plans = |at: Option<Ticks>| {
        s.ctx
            .temporal
            .lock()
            .expect("recorder")
            .replay_plan(at)
            .expect("a replay")
    };
    let show = |at| Show {
        session: 0,
        token: NO_TOKEN,
        plan: plans(at),
        judge: None,
        colors: None,
    };
    let screen = |term: &Terminal| {
        let text = crate::control::screen_text(term);
        (text, term.grid().scrollback_lines())
    };
    let mid = Some(ticks[23]);
    let (forks, bytes, from_cache) = warmed_show(plans(None), show(mid));
    assert!(
        forks.len() >= total / (FORK_EVERY + widest),
        "a fork every {FORK_EVERY} bytes: {forks:?}"
    );
    assert!(
        forks.windows(2).all(|w| w[0] < w[1]),
        "the forks are kept in order"
    );
    assert!(bytes > 0);
    let straight = show_inline(show(mid)).term;
    assert_eq!(
        screen(&from_cache.term),
        screen(&straight),
        "between forks the fold starts from the fork at or before its end"
    );
    assert!(
        screen(&from_cache.term).0.contains("chunk 23 line 49"),
        "negative control: it is the asked instant's screen, not a later fork's"
    );
    let (_, _, newest) = warmed_show(plans(None), show(None));
    assert_eq!(screen(&newest.term), screen(&show_inline(show(None)).term));
}

/// The fork cache holds at most [`MAX_FORKS`] forks and drops those farthest from the shown instant first.
#[test]
fn the_fork_cache_keeps_at_most_its_cap_and_drops_the_farthest_forks() {
    use crate::rewind_worker::{MAX_FORKS, STORE_LINES, kept_after};
    use aterm_buffer::Seq;
    let app = scripted_app();
    start_recording(&app);
    let _ = slice(&app, b"one screen");
    let plan = app
        .pool
        .get(0)
        .expect("session 0")
        .ctx
        .temporal
        .lock()
        .expect("recorder")
        .replay_plan(None)
        .expect("a replay");
    let engine = plan.restore_bounded(STORE_LINES);
    let seqs: Vec<Seq> = (0..40).map(|n| Seq(n * 10)).collect();
    let kept = kept_after(&engine, &seqs, Seq(390));
    assert_eq!(kept.len(), MAX_FORKS);
    assert_eq!(
        kept,
        (8..40).map(|n| Seq(n * 10)).collect::<Vec<_>>(),
        "the forks farthest from the shown instant go first"
    );
    let kept = kept_after(&engine, &seqs, Seq(0));
    assert_eq!(kept, (0..32).map(|n| Seq(n * 10)).collect::<Vec<_>>());
}

/// Drag then Cmd-C copies from the past screen, through the real pointer handlers.
#[test]
fn a_drag_over_the_past_screen_selects_and_copies_its_text() {
    let mut app = scripted_app();
    start_recording(&app);
    let past = slice(&app, b"\x1b[2J\x1b[Hthe past screen");
    let _ = slice(&app, b"\x1b[2J\x1b[Hthe live screen now");
    app.redraw_window(WID);
    app.rewind_open(0, Target::At(past), None).expect("open");
    app.redraw_window(WID);
    hover_across(&mut app, 0, 4, 0.1);
    app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
    hover_across(&mut app, 0, 7, 0.9);
    app.on_mouse_input(WID, ElementState::Released, MouseButton::Left);
    assert_eq!(
        app.window_selection_text(WID).as_deref(),
        Some("past"),
        "the selection is on the past screen"
    );
    let live = app.pool.get(0).expect("session 0").live_term().clone();
    assert!(
        !term_lock(&live).text_selection().has_selection(),
        "negative control: nothing is selected on the live screen"
    );
    assert_eq!(view(&app).picked, Some((0, 4)), "the press picked its cell");
}

/// Review fix #6 (R0b): Select All then Copy select and copy the past screen while it is shown.
#[test]
fn select_all_then_copy_takes_the_past_screen_not_the_live_one() {
    let copy_readiness = |app: &App| {
        app.publish_menu_live();
        let palette = app.palette_snapshot(WID);
        [
            crate::menu::menu_live().has_selection,
            app.palette_live().has_selection,
            palette
                .rows()
                .iter()
                .find(|row| row.action == crate::menu::MenuAction::Copy)
                .expect("Copy command")
                .enabled,
        ]
    };
    let terminal_state = |term: &aterm_core::terminal::Terminal| {
        (
            crate::control::screen_text(term),
            term.content_seq(),
            term.grid().display_offset(),
            term.selection_to_string(),
        )
    };
    let mut app = scripted_app();
    start_recording(&app);
    let past = slice(&app, b"\x1b[2J\x1b[Hthe past screen");
    let _ = slice(&app, b"\x1b[2J\x1b[Hthe live screen now");
    let live = app.pool.get(0).expect("session 0").live_term().clone();
    let live_unselected = terminal_state(&term_lock(&live));
    assert!(
        !term_lock(&live).text_selection().has_selection(),
        "negative control: the live screen has no selection"
    );
    app.rewind_open(0, Target::At(past), None).expect("open");
    app.select_all_in(WID);
    let copied = app.window_selection_text(WID).expect("a selection");
    assert!(
        copied.contains("the past screen") && !copied.contains("live"),
        "{copied:?}"
    );
    let shown = view(&app).shown.clone().expect("shown past screen");
    let past_selected = terminal_state(&term_lock(&shown));
    let selected_past_ready = copy_readiness(&app);
    assert_eq!(terminal_state(&term_lock(&shown)), past_selected);
    assert_eq!(terminal_state(&term_lock(&live)), live_unselected);
    assert_eq!(
        app.window_selection_text(WID).as_deref(),
        Some(copied.as_str())
    );

    assert!(app.rewind_close(0));
    app.select_all_in(WID);
    let copied = app.window_selection_text(WID).expect("a selection");
    assert!(
        copied.contains("the live screen now"),
        "negative control: live, it copies the live screen: {copied:?}"
    );
    let live_selected = terminal_state(&term_lock(&live));
    let selected_live_ready = copy_readiness(&app);
    assert!(term_lock(&live).text_selection().has_selection());

    app.rewind_open(0, Target::At(past), None).expect("reopen");
    let shown = view(&app).shown.clone().expect("reopened past screen");
    assert!(
        !term_lock(&shown).text_selection().has_selection(),
        "the reopened past screen has no selection"
    );
    assert_eq!(app.window_selection_text(WID), None);
    let past_unselected = terminal_state(&term_lock(&shown));
    let unselected_past_ready = copy_readiness(&app);
    assert_eq!(terminal_state(&term_lock(&shown)), past_unselected);
    assert_eq!(terminal_state(&term_lock(&live)), live_selected);
    assert_eq!(app.window_selection_text(WID), None);
    assert!(app.rewind_close(0));
    assert_eq!(terminal_state(&term_lock(&live)), live_selected);

    assert_eq!(
        [
            selected_past_ready,
            selected_live_ready,
            unselected_past_ready
        ],
        [[true; 3], [true; 3], [false; 3]],
        "native menu, palette predicate and Copy row follow the shown selection"
    );
}

/// Review fix #5 (R0b): find, its count and Cmd-G read the past screen while it is shown, and the live one again once live.
#[test]
fn find_counts_and_steps_over_the_past_screen_while_it_is_shown() {
    use crate::app_search::{FindAction, SearchEdit};
    let mut app = scripted_app();
    start_recording(&app);
    let past = slice(&app, b"\x1b[2J\x1b[Hneedle one\r\nneedle two");
    let _ = slice(&app, b"\x1b[2J\x1b[Hhaystack now");
    app.rewind_open(0, Target::At(past), None).expect("open");
    app.find_cmd(FindAction::Open).expect("front");
    let live_only = app
        .find_cmd(FindAction::Type("haystack".into()))
        .expect("front");
    assert_eq!(live_only.matches, 0, "the past holds no haystack");
    app.find_cmd(FindAction::Edit(SearchEdit::KillToStart));
    let found = app
        .find_cmd(FindAction::Type("needle".into()))
        .expect("front");
    assert_eq!(found.matches, 2, "the past's needles");
    let next = app
        .find_cmd(FindAction::Step { forward: true })
        .expect("front");
    assert_eq!(next.matches, 2);
    assert_ne!(next.current, found.current, "Cmd-G steps among them");
    let (row, ..) = next.current_match.expect("a current match");
    let line = shown_text(&app)
        .lines()
        .nth(usize::try_from(row).expect("on screen"))
        .map(str::to_owned);
    assert!(
        line.as_deref().is_some_and(|l| l.starts_with("needle")),
        "the match is on the shown screen: {line:?}"
    );

    assert!(app.rewind_close(0));
    let live = app.find_cmd(FindAction::Status).expect("front");
    assert_eq!(
        live.matches, 0,
        "negative control: live, the needles are gone from the screen"
    );
    app.find_cmd(FindAction::Edit(SearchEdit::KillToStart));
    let hay = app
        .find_cmd(FindAction::Type("haystack".into()))
        .expect("front");
    assert_eq!(hay.matches, 1, "and the live haystack is found");
}

/// Review fix #11 (R0b): with a rewound pane focused, the find bar and the in-grid rename field get their keys.
#[test]
fn the_find_bar_and_an_in_grid_rename_take_their_keys_over_a_rewound_pane() {
    use crate::app_search::FindAction;
    let mut app = scripted_app();
    app.tab_strip_rows = 1;
    start_recording(&app);
    let _ = slice(&app, b"past");
    let _ = slice(&app, b" live");
    app.rewind_open(0, Target::Latest, None).expect("open");
    assert!(
        key(
            &mut app,
            ModifiersState::empty(),
            &WinitKey::Character("x".into())
        ),
        "negative control: with nothing else open, Rewind withholds a letter"
    );

    app.find_cmd(FindAction::Open).expect("front");
    assert!(!key(
        &mut app,
        ModifiersState::empty(),
        &WinitKey::Character("x".into())
    ));
    assert!(!key(&mut app, ModifiersState::empty(), &arrow(true)));
    app.on_key(WID, character("x"));
    app.on_key(WID, character("y"));
    assert_eq!(
        app.windows[&WID].search.as_ref().map(|s| s.query.as_str()),
        Some("xy"),
        "typed into the find bar"
    );
    app.find_cmd(FindAction::Cancel).expect("front");
    assert!(app.pool.is_rewound(0), "still rewound");

    let tab = app.windows[&WID].tab_set.active_id().expect("a tab");
    assert!(app.begin_session_rename(WID, tab));
    assert!(
        app.inline_rename_edit(WID).is_some(),
        "fixture: an in-grid rename"
    );
    assert!(!key(
        &mut app,
        ModifiersState::empty(),
        &WinitKey::Character("x".into())
    ));
    app.on_key(WID, character("n"));
    assert_eq!(
        app.inline_rename_edit(WID).map(|e| e.text.as_str()),
        Some("n"),
        "typed into the rename field"
    );
    app.end_session_rename_editor(WID);
    assert!(
        key(
            &mut app,
            ModifiersState::empty(),
            &WinitKey::Character("x".into())
        ),
        "both closed: Rewind withholds again"
    );
}

/// Review fix #13 (R0b): no link or hover is probed against the live screen over a rewound pane.
#[test]
fn no_live_link_is_hovered_or_opened_over_a_rewound_pane() {
    const URL: &str = "https://evil.example/steal";
    let mut app = scripted_app();
    start_recording(&app);
    let past = slice(&app, b"\x1b[2J\x1b[Hplain text in the past, no link at all");
    let _ = slice(
        &app,
        format!("\x1b[2J\x1b[HVisit \x1b]8;;{URL}\x1b\\google.com\x1b]8;;\x1b\\ now").as_bytes(),
    );
    app.on_modifiers_changed(WID, link_modifier());
    app.redraw_window(WID);
    hover(&mut app, 0, 7);
    assert!(
        app.windows[&WID].link_hover.is_some(),
        "fixture: live, the link is hovered"
    );
    assert_eq!(app.link_under_pointer(WID).as_deref(), Some(URL), "fixture");

    app.rewind_open(0, Target::At(past), None).expect("open");
    assert!(
        app.windows[&WID].link_hover.is_none(),
        "the live hover goes when the view opens"
    );
    app.redraw_window(WID);
    hover(&mut app, 0, 8);
    hover(&mut app, 0, 7);
    assert!(
        app.windows[&WID].link_hover.is_none(),
        "no hover over the past"
    );
    assert_eq!(app.link_under_pointer(WID), None, "no live link to open");

    assert!(app.rewind_close(0));
    app.redraw_window(WID);
    hover(&mut app, 0, 8);
    hover(&mut app, 0, 7);
    assert_eq!(
        app.link_under_pointer(WID).as_deref(),
        Some(URL),
        "negative control: live again, the link is offered"
    );
}

/// Pick a cell by a press on it, then B goes to the slice that last changed it, then to the change before, then to the oldest state.
#[test]
fn b_goes_to_the_slice_that_last_changed_the_picked_cell_and_on_back() {
    let mut app = scripted_app();
    start_recording(&app);
    let oldest = instants(&app)[0].at;
    let x = slice(&app, b"\x1b[1;1Hx");
    let y = slice(&app, b"\x1b[2;1Hy");
    let z = slice(&app, b"\x1b[1;1Hz");
    let _ = slice(&app, b"\x1b[3;1Hw");
    app.rewind_open(0, Target::Latest, None).expect("open");
    app.redraw_window(WID);
    assert_eq!(
        app.rewind_changed(0),
        Err("click a cell first, then press B".to_string())
    );
    hover(&mut app, 0, 0);
    app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(WID, ElementState::Released, MouseButton::Left);
    assert_eq!(view(&app).picked, Some((0, 0)));

    let b = WinitKey::Character("b".into());
    assert!(key(&mut app, ModifiersState::empty(), &b));
    assert_eq!(view(&app).at, z, "the cell last changed to z");
    let note = view(&app).note.clone().unwrap_or_default();
    assert!(
        note.starts_with("row 1 col 1 changed ") && note.ends_with(" s ago: 7 bytes \"\\e[1;1Hz\""),
        "{note:?}"
    );
    assert!(key(&mut app, ModifiersState::empty(), &b));
    assert_eq!(view(&app).at, x, "and before that to x");
    assert!(key(&mut app, ModifiersState::empty(), &b));
    assert_eq!(
        view(&app).at,
        oldest,
        "and before that it was as at the oldest state"
    );
    assert!(
        view(&app)
            .note
            .as_deref()
            .is_some_and(|n| n.starts_with("row 1 col 1 is as at the oldest kept state, ")),
        "{:?}",
        view(&app).note
    );

    use crate::rewind_verb::Asked;
    let _ = ask(&mut app, Asked::Open(Target::Latest)).expect("open");
    let other = ask(&mut app, Asked::Changed(1, 0)).expect("changed");
    assert!(
        other.contains(&format!(" at={} ", y.0))
            && other.ends_with(" cell=1,0 changed=yes bytes=7"),
        "negative control: another cell's change is its own: {other}"
    );
    let untouched = ask(&mut app, Asked::Changed(5, 10)).expect("changed");
    assert!(
        untouched.contains(&format!(" at={} ", oldest.0))
            && untouched.ends_with(" cell=5,10 changed=no bytes=0"),
        "negative control: a cell nothing wrote never changed: {untouched}"
    );
}

/// Opening Rewind pauses the window's hosted effects and the band says so; closing resumes only what it paused.
#[test]
fn opening_rewind_pauses_the_windows_hosted_effects_and_closing_resumes_them() {
    let mut app = scripted_app();
    assert_eq!(
        app.rewind_effects_status(WID),
        None,
        "fixture: none attached"
    );
    start_recording(&app);
    let _ = slice(&app, b"past");
    app.rewind_open(0, Target::Latest, None).expect("open");
    assert!(
        view(&app).paused.is_empty(),
        "nothing attached, nothing paused"
    );
    app.redraw_window(WID);
    let (rows, _) = grid(&app);
    let band = text(&drawn(&app)[rows - 1..]);
    assert!(!band.contains("effects"), "{band:?}");
}

#[cfg(a11y_tree)]
#[test]
fn a_rewound_pane_is_an_accessibility_slider_over_its_states() {
    use aterm_a11y::model::{Action, ActionRequest, Role, TreeId};
    let mut app = scripted_app();
    start_recording(&app);
    let _ = slice(&app, b"one");
    let _ = slice(&app, b"\r\ntwo");
    app.redraw_window(WID);
    let tree = app.grid_a11y_tree(WID, None).expect("a tree");
    assert!(
        !tree
            .nodes
            .iter()
            .any(|(id, _)| *id == crate::a11y_tree::REWIND),
        "negative control: live, no slider"
    );
    app.rewind_open(0, Target::Latest, None).expect("open");
    app.redraw_window(WID);
    let tree = app.grid_a11y_tree(WID, None).expect("a tree");
    let slider = &tree
        .nodes
        .iter()
        .find(|(id, _)| *id == crate::a11y_tree::REWIND)
        .expect("the slider")
        .1;
    assert_eq!(slider.role(), Role::Slider);
    assert_eq!(slider.label(), Some("Rewind"));
    assert_eq!(slider.numeric_value(), Some(3.0));
    assert_eq!(slider.max_numeric_value(), Some(3.0));
    assert!(
        slider
            .description()
            .is_some_and(|d| d.contains("unchecked")),
        "{:?}",
        slider.description()
    );
    app.on_accessibility_action(
        WID,
        ActionRequest {
            context: None,
            action: Action::Decrement,
            target_tree: TreeId::ROOT,
            target_node: crate::a11y_tree::REWIND,
            data: None,
        },
    );
    assert_eq!(view(&app).state, (2, 3), "Decrement steps back");
}

/// `rewind export`'s cast paints the screen the recording began on, then plays every slice: played into a fresh engine, it ends on the live screen.
#[test]
fn an_exported_cast_plays_back_to_the_live_screen() {
    let app = scripted_app();
    {
        let live = app.pool.get(0).expect("session 0").live_term().clone();
        term_lock(&live).process(b"on screen before recording\r\n");
    }
    start_recording(&app);
    let _ = slice(&app, b"one\r\n");
    let _ = slice(&app, b"\x1b[31mtwo\x1b[m");
    let export = {
        let s = app.pool.get(0).expect("session 0");
        s.ctx.temporal.lock().expect("recorder").export()
    }
    .expect("an export");
    let (cast, events) = crate::rewind_verb::cast_of(&export);
    assert_eq!(events, 3, "the painted base and two slices");
    let parsed = aterm_control::asciicast::parse(&cast).expect("a cast");
    let (rows, cols) = (parsed.header.height, parsed.header.width);
    let play = |events: &[aterm_control::asciicast::CastEvent<Vec<u8>>]| {
        let mut term = Terminal::new(rows, cols);
        for ev in events {
            if let aterm_control::asciicast::CastEvent::Output { data, .. } = ev {
                term.process(data);
            }
        }
        crate::control::screen_text(&term)
    };
    assert_eq!(play(&parsed.events), live_text(&app));
    assert_ne!(
        play(&parsed.events[1..]),
        live_text(&app),
        "negative control: without the painted base the screen before recording is missing"
    );
}

#[cfg(unix)]
#[test]
fn an_export_is_written_private_and_never_over_a_file() {
    use std::os::unix::fs::PermissionsExt;
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let into = dir.path().join("rewind");
    let path = crate::rewind_verb::write_private(&into, "a.cast", b"cast").expect("written");
    let meta = std::fs::metadata(&path).expect("written");
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    assert_eq!(
        std::fs::metadata(&into).expect("dir").permissions().mode() & 0o777,
        0o700
    );
    assert!(crate::rewind_verb::write_private(&into, "a.cast", b"other").is_err());
    assert_eq!(std::fs::read(&path).expect("read"), b"cast");
}

#[cfg(unix)]
#[test]
fn a_saved_export_is_private_and_leaves_the_chosen_folders_mode_alone() {
    use std::os::unix::fs::PermissionsExt;
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let chosen = dir.path().join("Documents");
    std::fs::create_dir(&chosen).expect("folder");
    std::fs::set_permissions(&chosen, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let path = chosen.join("history.cast");
    crate::rewind_verb::write_chosen(&path, b"cast").expect("written");
    let mode =
        |p: &std::path::Path| std::fs::metadata(p).expect("stat").permissions().mode() & 0o777;
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(&chosen), 0o755, "the person's folder keeps its mode");
    crate::rewind_verb::write_chosen(&path, b"again").expect("the panel asked before replacing");
    assert_eq!(std::fs::read(&path).expect("read"), b"again");
    assert_eq!(mode(&path), 0o600);
}

/// The frames a person sees, as PNGs for them to read.
#[test]
#[ignore = "writes PNGs for a person to read: set ATERM_REWIND_GALLERY to a directory"]
fn rewind_gallery() {
    let Some(dir) = std::env::var_os("ATERM_REWIND_GALLERY") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("the gallery directory");
    let mut app = scripted_app();
    start_recording(&app);
    // Spaced so the track spreads the instants over its row.
    let paced = |app: &App, bytes: &[u8]| {
        std::thread::sleep(std::time::Duration::from_millis(40));
        let _ = slice(app, bytes);
    };
    paced(&app, FIRST_PROMPT);
    paced(&app, b"cargo test\r\n");
    paced(&app, b"\x1b]133;C\x07running 42 tests\r\n");
    paced(&app, b"test rewind::steps ... \x1b[32mok\x1b[m\r\n");
    let live = app.pool.get(0).expect("session 0").live_term().clone();
    witness(&app, &term_lock(&live));
    paced(&app, b"test rewind::finds ... \x1b[31mFAILED\x1b[m\r\n");
    let mut stranger = Terminal::new(24, 80);
    stranger.process(b"not what was on screen");
    witness(&app, &stranger);
    paced(&app, PROMPT);
    paced(&app, b"ls\r\n");
    let shoot = |app: &mut App, name: &str| {
        app.redraw_window(WID);
        let png = crate::scripted_redraw_fixture::headless(app, |app| {
            let (reply, rx) = std::sync::mpsc::channel();
            app.render_image(crate::control::ImageReq {
                handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
                target: crate::control_auth::ConfinedImage::in_memory(),
                clean: false,
                session: None,
                want_bytes: true,
                want_metadata: false,
                frame_metadata: Arc::new(std::sync::OnceLock::new()),
                cancel: crate::control::CaptureCancellation::new(),
                reply,
            });
            rx.recv_timeout(std::time::Duration::from_secs(60))
                .expect("image reply")
                .expect("image succeeds")
                .value
                .2
                .expect("PNG bytes")
        });
        std::fs::write(dir.join(format!("{name}.png")), png).expect("a PNG");
    };
    shoot(&mut app, "live");
    app.rewind_open(0, Target::Latest, None).expect("open");
    shoot(&mut app, "rewound_newest");
    assert!(key(&mut app, ModifiersState::SHIFT, &arrow(true)));
    shoot(&mut app, "command_start_faithful");
    assert!(key(&mut app, ModifiersState::empty(), &arrow(false)));
    assert!(key(&mut app, ModifiersState::empty(), &arrow(false)));
    shoot(&mut app, "may_differ_hatched");
    hover(&mut app, 2, 25);
    app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(WID, ElementState::Released, MouseButton::Left);
    assert!(key(
        &mut app,
        ModifiersState::empty(),
        &WinitKey::Character("b".into())
    ));
    shoot(&mut app, "changed_cell_note");
}

/// A recording tab's menu timeline row opens Rewind on that tab at the instant it names.
#[test]
fn a_tab_menu_timeline_row_opens_rewind_at_its_instant() {
    use crate::session_chrome::TabMenuEntry;
    let mut app = scripted_app();
    app.tab_strip_rows = 1;
    start_recording(&app);
    let before = slice(&app, b"before the cd");
    std::thread::sleep(std::time::Duration::from_millis(20));
    let s = app.pool.get(0).expect("session 0");
    s.ctx
        .timeline
        .lock()
        .expect("timeline")
        .record_hand_row("cwd-change", "cwd=/tmp".into());
    std::thread::sleep(std::time::Duration::from_millis(20));
    let _ = slice(&app, b"\r\nafter the cd");
    assert!(app.open_active_tab_context_menu(WID));
    let entries = app.windows[&WID]
        .tab_menu
        .as_ref()
        .expect("the menu is open")
        .entries
        .clone();
    let row = entries
        .iter()
        .position(|e| {
            matches!(e, TabMenuEntry::RewindTo { words, .. } if words.starts_with("changed directory"))
        })
        .unwrap_or_else(|| panic!("a Rewind row for the cd: {entries:?}"));
    assert!(crate::tab_menu::is_selectable(&entries[row]));
    app.activate_tab_menu_entry(WID, row);
    assert!(app.windows[&WID].tab_menu.is_none(), "chosen and closed");
    assert_eq!(
        view(&app).at,
        before,
        "the screen as it was when the cd happened"
    );
    assert!(shown_text(&app).starts_with("before the cd") && !shown_text(&app).contains("after"));
}

/// Session 0's view with a fold thread that posts each fold to the test, so a fold stays in flight until the test hands it back.
fn held_view(app: &mut App) -> std::sync::mpsc::Receiver<Folded> {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut view = RewindView::new(0);
    view.worker = Some(
        RewindWorker::start(move |done| match done {
            Done::Shown(folded) => tx.send(*folded).is_ok(),
            Done::Judged { .. } | Done::Changed(_) => true,
        })
        .expect("a fold thread"),
    );
    assert!(
        app.pool.set_rewind(0, Some(view)).is_none(),
        "fixture: no view was open"
    );
    rx
}

/// The next fold the held thread posts (a one-minute hang detector).
fn posted(rx: &std::sync::mpsc::Receiver<Folded>) -> Folded {
    rx.recv_timeout(std::time::Duration::from_secs(60))
        .expect("the fold thread posts its fold")
}

/// A fold a closed view left in flight is never shown by the view opened after it, though both views count their folds from 1.
#[test]
fn a_closed_views_late_fold_is_never_shown_by_the_view_opened_after_it() {
    let mut app = scripted_app();
    start_recording(&app);
    let first = slice(&app, b"\x1b[2J\x1b[Hthe first screen");
    let second = slice(&app, b"\x1b[2J\x1b[Hthe second screen");
    let closed = held_view(&mut app);
    app.rewind_open(0, Target::At(first), None).expect("open");
    let late = posted(&closed);
    assert!(app.rewind_close(0));
    let open = held_view(&mut app);
    app.rewind_open(0, Target::At(second), None)
        .expect("reopen");
    let own = posted(&open);
    assert_eq!(
        late.token.serial, own.token.serial,
        "fixture: each view counts its folds from the same start"
    );
    app.rewind_folded(late);
    let view = app.pool.rewind(0).expect("the open view");
    assert!(
        view.shown.is_none() && view.folding.is_some(),
        "the closed view's fold is dropped and the open view's own is still awaited"
    );
    app.rewind_folded(own);
    assert_eq!(app.pool.rewind(0).expect("the open view").at, second);
    assert!(
        shown_text(&app).starts_with("the second screen"),
        "{:?}",
        shown_text(&app)
    );
}

/// Steps asked while a fold is in flight wait as one fold to the newest of them, every reply answers once its fold is shown, and a close answers the rest with an error.
#[test]
fn steps_asked_during_a_fold_fold_once_to_the_newest_and_every_reply_answers() {
    let mut app = scripted_app();
    start_recording(&app);
    let a = slice(&app, b"a");
    let _ = slice(&app, b"b");
    let c = slice(&app, b"c");
    let held = held_view(&mut app);
    let reply = std::sync::mpsc::channel::<Result<String, String>>;
    let waiting = |rx: &std::sync::mpsc::Receiver<Result<String, String>>| {
        matches!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty))
    };
    let (opened_tx, opened) = reply();
    app.rewind_open(0, Target::At(c), Some(opened_tx))
        .expect("open");
    let (step1_tx, step1) = reply();
    let (step2_tx, step2) = reply();
    app.rewind_step(0, StepBy::Slice(-1), Some(step1_tx))
        .expect("step");
    app.rewind_step(0, StepBy::Slice(-1), Some(step2_tx))
        .expect("step");
    let first = posted(&held);
    assert!(held.try_recv().is_err(), "one fold in flight");
    assert!(
        waiting(&opened) && waiting(&step1) && waiting(&step2),
        "nothing answers before a fold is shown"
    );
    app.rewind_folded(first);
    let line = opened.try_recv().expect("the open answers").expect("open");
    assert!(line.contains(&format!(" at={} state=4/4 ", c.0)), "{line}");
    assert!(
        waiting(&step1) && waiting(&step2),
        "the steps wait on their own fold"
    );
    app.rewind_folded(posted(&held));
    for step in [&step1, &step2] {
        let line = step.try_recv().expect("the step answers").expect("step");
        assert!(
            line.contains(&format!(" at={} state=2/4 ", a.0)),
            "both steps answer with the newest step's state: {line}"
        );
    }
    let (step3_tx, step3) = reply();
    let (step4_tx, step4) = reply();
    app.rewind_step(0, StepBy::Slice(1), Some(step3_tx))
        .expect("step");
    app.rewind_step(0, StepBy::Slice(1), Some(step4_tx))
        .expect("step");
    let _in_flight = posted(&held);
    assert!(app.rewind_close(0));
    for step in [&step3, &step4] {
        assert!(
            step.try_recv()
                .expect("a close answers every waiting reply")
                .is_err(),
            "with an error"
        );
    }
}

/// At 39 columns, half of an 80-column window split side by side, the band still says the screen may differ, in its warning ink.
#[test]
fn a_narrow_band_keeps_the_verdict_in_its_warning_ink() {
    let inks = crate::chrome_band::band_colors(aterm_render::Theme::default());
    assert_ne!(inks.warn, inks.value, "fixture: a warning has its own ink");
    let view = view_at((803, 1412), Faith::MayDiffer);
    let text = band_text(&view, 1_042_000, 0, 1_000 + 5_000, None);
    let row = crate::rewind_band::band_row(&view, 39, &inks, &text);
    let drawn: String = row.iter().map(|c| c.ch).collect();
    let at = drawn
        .find("may differ")
        .unwrap_or_else(|| panic!("the verdict is cut off: {drawn:?}"));
    let col = drawn[..at].chars().count();
    for (i, ch) in "may differ".chars().enumerate() {
        assert_eq!(
            row[col + i],
            crate::chrome_band::cell(ch, inks.warn, inks.bar_bg, true, false),
            "{drawn:?}"
        );
    }
}

/// The band's clock and `rewind open ago=` count time the machine slept: a screen from before a sleep shows the time it was on screen.
#[test]
fn a_sleep_counts_toward_the_shown_time_and_toward_ago() {
    use crate::rewind_verb::Asked;
    const SLEPT: std::time::Duration = std::time::Duration::from_secs(2 * 3600 + 5 * 60);
    let mut app = scripted_app();
    start_recording(&app);
    let before = slice(&app, b"before the lid closed");
    app.pool
        .get(0)
        .expect("session 0")
        .ctx
        .temporal
        .lock()
        .expect("recorder")
        .sleep_for_test(SLEPT);
    let _ = slice(&app, b"\r\nafter it opened");
    let open = ask(&mut app, Asked::Open(Target::Ago(600))).expect("ten minutes ago is mid-sleep");
    assert!(open.contains(&format!(" at={} ", before.0)), "{open}");
    let shown = app.pool.rewind(0).expect("the view").shown_unix_ms;
    let behind = crate::harness_screen::unix_ms_now() - shown;
    let slept = i64::try_from(SLEPT.as_millis()).expect("ms");
    assert!(
        (slept..slept + 60_000).contains(&behind),
        "shown {behind} ms ago across a {slept} ms sleep"
    );
}

/// The band's and the track's rows are chrome: the past screen's cursor on them is not drawn over the band, and a pointer there is over chrome.
#[test]
fn the_band_row_is_chrome_that_the_past_cursor_does_not_draw_over() {
    let mut app = scripted_app();
    app.redraw_window(WID);
    start_recording(&app);
    let (rows, _) = grid(&app);
    let filled: String = (0..rows + 5).map(|i| format!("\r\nline {i}")).collect();
    let past = slice(&app, filled.as_bytes());
    let _ = slice(&app, b"\x1b[2J\x1b[Hlive");
    app.redraw_window(WID);
    assert!(
        app.windows[&WID].input_scratch.cursor_visible,
        "fixture: a live frame draws the cursor"
    );
    app.rewind_open(0, Target::At(past), None).expect("open");
    let shown = app.rewind_shown(0).expect("shown");
    assert_eq!(
        usize::from(term_lock(&shown).cursor().row),
        rows - 1,
        "fixture: the past cursor is on the band's row"
    );
    app.redraw_window(WID);
    assert!(
        !app.windows[&WID].input_scratch.cursor_visible,
        "the past cursor is not drawn over the band"
    );
    assert!(
        app.chrome_owns_terminal_row(WID, rows - 1),
        "the band's row is chrome"
    );
    assert!(
        app.chrome_owns_terminal_row(WID, rows - 2),
        "so is the track's above it"
    );
    assert!(
        !app.chrome_owns_terminal_row(WID, rows - 3),
        "the past screen's rows are not"
    );
}

/// The fold draws in the session's colours and keeps the cursor the program set: hidden, a steady bar.
#[test]
fn the_fold_draws_in_the_sessions_colours_and_keeps_the_programs_cursor() {
    let app = scripted_app();
    start_recording(&app);
    let at = slice(&app, b"\x1b[?25l\x1b[6 qa hidden bar");
    let colors = aterm_core::config::TerminalConfig {
        default_background: aterm_core::terminal::Rgb {
            r: 250,
            g: 250,
            b: 240,
        },
        ..aterm_core::config::TerminalConfig::default()
    };
    let plan = app
        .pool
        .get(0)
        .expect("session 0")
        .ctx
        .temporal
        .lock()
        .expect("recorder")
        .replay_plan(Some(at))
        .expect("a replay");
    let folded = crate::rewind_worker::show_inline(crate::rewind_worker::Show {
        session: 0,
        token: NO_TOKEN,
        plan,
        judge: None,
        colors: Some(Arc::new(colors.clone())),
    })
    .term;
    let live = app.pool.get(0).expect("session 0").live_term().clone();
    let cursor = |t: &Terminal| (t.cursor_visible(), t.cursor_style());
    assert_eq!(
        cursor(&term_lock(&live)),
        (false, aterm_core::terminal::CursorStyle::SteadyBar),
        "fixture"
    );
    assert_eq!(
        cursor(&folded),
        cursor(&term_lock(&live)),
        "the program's cursor"
    );
    assert_eq!(
        folded.default_background(),
        colors.default_background,
        "the session's colours"
    );
}

/// A fold to an instant inside an open mode-2026 frame whose slice ended mid-character ends the frame without forcing out a replacement glyph, and says it ended one.
#[test]
fn a_fold_inside_an_open_frame_forces_out_no_replacement_glyph() {
    let app = scripted_app();
    start_recording(&app);
    let open = slice(&app, b"\x1b[2J\x1b[H\x1b[?2026hab\xe2\x94");
    let _ = slice(&app, b"\x80\x1b[?2026l");
    assert!(
        !instants(&app).iter().any(|i| i.at == open),
        "fixture: a step never lands inside the frame"
    );
    let plan = app
        .pool
        .get(0)
        .expect("session 0")
        .ctx
        .temporal
        .lock()
        .expect("recorder")
        .replay_plan(Some(open))
        .expect("a replay");
    let folded = crate::rewind_worker::show_inline(crate::rewind_worker::Show {
        session: 0,
        token: NO_TOKEN,
        plan,
        judge: None,
        colors: None,
    });
    assert!(folded.mid_frame, "the fold ended the open frame");
    let shown = crate::control::screen_text(&folded.term);
    assert!(
        !shown.contains('\u{FFFD}') && shown.starts_with("ab\n"),
        "{shown:?}"
    );
}

/// A shown fold that had to end a mode-2026 frame reads unchecked though the witness on its instant matched: its half-drawn grid was never on screen.
#[test]
fn a_fold_that_ended_an_open_frame_reads_unchecked_though_its_witness_matched() {
    use crate::rewind_verb::Asked;
    for mid_frame in [true, false] {
        let mut app = scripted_app();
        start_recording(&app);
        let shown = slice(&app, b"\x1b[2J\x1b[Ha whole frame");
        let live = app.pool.get(0).expect("session 0").live_term().clone();
        witness(&app, &term_lock(&live));
        let (tx, rx) = std::sync::mpsc::channel();
        let mut view = RewindView::new(0);
        view.worker =
            Some(RewindWorker::start(move |done| tx.send(done).is_ok()).expect("a fold thread"));
        assert!(app.pool.set_rewind(0, Some(view)).is_none(), "fixture");
        app.rewind_open(0, Target::At(shown), None).expect("open");
        let next = || {
            rx.recv_timeout(std::time::Duration::from_secs(60))
                .expect("the fold thread posts")
        };
        let Done::Shown(mut folded) = next() else {
            panic!("the fold comes first");
        };
        folded.mid_frame = mid_frame;
        app.rewind_done(Done::Shown(folded));
        let judged = next();
        assert!(matches!(judged, Done::Judged { .. }), "{judged:?}");
        app.rewind_done(judged);
        let fidelity = app
            .pool
            .get(0)
            .expect("session 0")
            .ctx
            .temporal
            .lock()
            .expect("recorder")
            .fidelity_at(shown);
        assert_eq!(fidelity, Fidelity::Faithful, "fixture: the witness matched");
        let status = ask(&mut app, Asked::Status).expect("status");
        let want = if mid_frame {
            " faithful=unchecked "
        } else {
            " faithful=yes "
        };
        assert!(
            status.contains(want),
            "mid_frame={mid_frame} (false: the negative control): {status}"
        );
    }
}

/// A paste into the command palette over a rewound pane reaches the palette, not the program, and is not called withheld.
#[cfg(unix)]
#[test]
fn a_paste_into_the_palette_over_a_rewound_pane_reaches_the_palette() {
    let (mut app, mut program) = piped();
    start_recording(&app);
    let _ = slice(&app, b"$ ");
    app.rewind_open(0, Target::Latest, None).expect("open");
    app.palette_enter();
    crate::control::PBPASTE_STUB.with(|board| *board.borrow_mut() = Some("ab".to_string()));
    app.paste_clipboard();
    crate::control::PBPASTE_STUB.with(|board| *board.borrow_mut() = None);
    assert_eq!(
        app.windows[&WID].palette().expect("the palette").query(),
        "ab"
    );
    assert!(
        app.messages.live_by_key(&read_only_key(0)).is_none(),
        "nothing was withheld"
    );
    assert!(
        program_read(&mut program).is_empty(),
        "nothing reached the program"
    );
}

/// A rewound Claude Code pane gets no footer glass or targets, and the context reading on its past screen never becomes the live agent's.
#[test]
fn a_rewound_claude_pane_lays_no_footer_and_reads_its_past_into_no_live_fact() {
    use crate::claude_footer::probe;
    let context = |app: &App| {
        let s = app.pool.get(0).expect("session 0");
        let timeline = s
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        timeline.claude_footer().and_then(|f| f.context.clone())
    };
    let footer = |app: &mut App| {
        for _ in 0..3 {
            probe::refill(app, WID);
            let _ = probe::pass(app, WID);
        }
        let ws = &app.windows[&WID];
        let strip = ws.input_scratch.cells.len() - usize::from(ws.rows);
        let rule = crate::claude_footer::read_glass(
            &ws.input_scratch.cells[strip + probe::AT_REST.bottom()],
        );
        let targets = ws.claude_lights.fact_hits.len();
        (rule, targets)
    };
    let mut app = App::headless_for_test();
    assert_eq!(
        app.focused_session_id(WID),
        Some(0),
        "fixture: the recording helpers drive session 0"
    );
    assert!(app.apply_term_resize(WID, probe::ROWS, probe::COLS));
    probe::publish(&app, 0, probe::facts());
    {
        let s = app.pool.get(0).expect("session 0");
        let mut timeline = s
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        assert!(timeline.set_program(4242, Some("claude".into())));
    }
    start_recording(&app);
    let past = slice(&app, probe::paint(&probe::context_screen(61)).as_bytes());
    let _ = slice(&app, probe::paint(&probe::context_screen(7)).as_bytes());
    let (rule, targets) = footer(&mut app);
    assert!(
        rule.contains("\u{25D0} ctx 7% left") && targets > 0,
        "CONTROL: the live pane's footer is drawn and laid ({targets} targets): {rule:?}"
    );
    assert_eq!(
        context(&app).as_deref(),
        Some("ctx 7% left"),
        "CONTROL: the live reading"
    );
    app.rewind_open(0, Target::At(past), None).expect("open");
    assert!(
        shown_text(&app).contains("61% until auto-compact"),
        "fixture: the past shows its own reading"
    );
    let (rule, targets) = footer(&mut app);
    assert!(
        !rule.contains("ctx") && targets == 0,
        "no footer over the past screen ({targets} targets): {rule:?}"
    );
    assert_eq!(
        context(&app).as_deref(),
        Some("ctx 7% left"),
        "the past screen's reading is not the live agent's"
    );
}

/// D13 on one pane: neither a present nor a capture draws Sparkle Words over a rewound pane, though its past screen holds a word the field never lit, and once live each draws words again.
#[test]
fn a_rewound_pane_draws_no_sparkle_words_in_a_present_or_a_capture() {
    use std::time::{Duration, Instant};
    for capture in [false, true] {
        let route = if capture { "capture" } else { "present" };
        let mut app = App::headless_for_test();
        app.config.motion = Some("full".into());
        {
            let ws = app.windows.get_mut(&WID).expect("window");
            ws.focused = true;
            ws.present = Some(crate::PresentTarget::Cpu {
                surface: crate::present::CpuSurface::Scripted(Default::default()),
            });
        }
        app.recompute_sparkle();
        assert!(app.sparkle.is_some(), "fixture: Sparkle Words is on");
        let t0 = Instant::now();
        let at = |k: u64| t0 + Duration::from_millis(16 * k);
        let drawn = |app: &mut App, frames: std::ops::Range<u64>| {
            frames
                .map(|k| {
                    if capture {
                        app.capture_words(WID, at(k));
                    } else {
                        app.redraw_window_at(WID, at(k));
                    }
                    let input = &app.windows[&WID].input_scratch;
                    input.word_decorations.len() + input.ink.len()
                })
                .max()
                .unwrap_or(0)
        };
        start_recording(&app);
        let past = slice(&app, b"\x1b[2J\x1b[3;5Hwhat the fuck\r\n");
        let _ = slice(
            &app,
            b"\x1b[2J\x1b[Hthe live screen, hello kitty friend\r\n",
        );
        assert!(
            drawn(&mut app, 0..120) > 0,
            "CONTROL: a live {route} draws the words"
        );
        for line in 0..4 {
            let _ = slice(&app, format!("live line {line}\r\n").as_bytes());
            let _ = drawn(&mut app, 120 + line..121 + line);
        }

        app.rewind_open(0, Target::At(past), None).expect("open");
        assert!(
            shown_text(&app).contains("what the fuck"),
            "fixture: the past shows words to decorate"
        );
        let live = term_lock(app.pool.get(0).expect("session 0").live_term()).damage_epoch();
        let shown = term_lock(&app.rewind_shown(0).expect("a shown screen")).damage_epoch();
        assert_ne!(
            live, shown,
            "fixture: the past is a grid the field has not scanned"
        );
        assert_eq!(
            drawn(&mut app, 130..250),
            0,
            "no {route} draws words over a rewound pane"
        );

        assert!(app.rewind_key(WID, ModifiersState::empty(), &esc(), false));
        let _ = drawn(&mut app, 250..260);
        let _ = slice(&app, b"\x1b[2J\x1b[5;20Hwhat the fuck kitty\r\n");
        assert!(
            drawn(&mut app, 260..380) > 0,
            "CONTROL: live again, a {route} draws the new words"
        );
    }
}

/// D13 on a composed split: the rewound pane gets no Sparkle Words and no output streak, while the live pane beside it keeps its own.
#[test]
fn a_rewound_split_pane_composes_no_words_and_no_streak() {
    use std::time::{Duration, Instant};
    let mut app = App::headless_for_test();
    app.config.motion = Some("full".into());
    app.config.output_streak = Some(crate::app_config::OutputStreakConfig {
        enabled: Some(true),
        sound: Some(false),
        ..Default::default()
    });
    let other = app.split_active_stub_tab(WID);
    app.windows.get_mut(&WID).expect("window").focused = true;
    app.recompute_sparkle();
    assert!(app.sparkle.is_some(), "fixture: Sparkle Words is on");
    let plan = app.active_visible_leaf_plan(WID).expect("a visible plan");
    let (left, width) = plan
        .leaves
        .iter()
        .find(|leaf| {
            app.view_store
                .get(leaf.view)
                .copied()
                .and_then(crate::tab_model::View::terminal_session)
                == Some(0)
        })
        .map(|leaf| {
            (
                leaf.rect.origin.x.round() as u16,
                leaf.rect.size.width.round() as u16,
            )
        })
        .expect("session 0's pane");
    let mine = left..left + width;
    let cw = app.win_cell_size(WID).0 as i32;
    let mine_px = i32::from(left) * cw..i32::from(left + width) * cw;
    let t0 = Instant::now();
    let at = |k: u64| t0 + Duration::from_millis(16 * k);
    let frame = |app: &mut App, k: u64| {
        let (rows, cols) = {
            let ws = &app.windows[&WID];
            (usize::from(ws.rows), usize::from(ws.cols))
        };
        let composed = app
            .redraw_compose(WID, rows, cols, false, false, None, 0, at(k))
            .is_some();
        let input = &app.windows[&WID].input_scratch;
        let words = input.ink.iter().filter(|c| mine.contains(&c.col)).count()
            + input
                .word_decorations
                .iter()
                .filter(|d| mine.contains(&d.col))
                .count()
            + aterm_render::sprites::published(&input.fx_layers)
                .iter()
                .filter(|s| mine_px.contains(&s.visible().x0))
                .count();
        (words, app.windows[&WID].fx.streak_panes().count(), composed)
    };
    let feed_other = |app: &App, bytes: &[u8]| {
        let term = app.pool.get(other).expect("the other pane").term.clone();
        term_lock(&term).process(bytes);
    };
    let bg = aterm_render::Theme::default().bg;
    for s in [0, other] {
        term_lock(&app.pool.get(s).expect("a pane").term).set_default_background(
            aterm_core::terminal::Rgb {
                r: (bg >> 16) as u8,
                g: (bg >> 8) as u8,
                b: bg as u8,
            },
        );
    }
    let _ = frame(&mut app, 0);
    start_recording(&app);
    let past = slice(&app, b"\r\nhello kitty friend\r\n");
    feed_other(&app, b"\r\nhello kitty friend\r\n");
    app.windows
        .get_mut(&WID)
        .expect("window")
        .pending_deco_birth = Some(at(1));
    let live: Vec<_> = (1..120).map(|k| frame(&mut app, k)).collect();
    assert!(
        live.iter().any(|(words, _, _)| *words > 0),
        "CONTROL: session 0's live pane composes words: {live:?}"
    );
    assert!(
        live.iter().any(|(_, streaks, _)| *streaks == 2),
        "CONTROL: both live panes run a streak: {live:?}"
    );
    let _ = slice(&app, b"the live screen, hello kitty friend\r\n");

    app.rewind_open(0, Target::At(past), None).expect("open");
    feed_other(&app, b"more output, hello kitty friend\r\n");
    let rewound: Vec<_> = (120..240).map(|k| frame(&mut app, k)).collect();
    assert!(
        rewound
            .iter()
            .all(|(words, _, composed)| *words == 0 || !*composed)
            && rewound.iter().any(|(_, _, composed)| *composed),
        "no words composed over the rewound pane: {rewound:?}"
    );
    assert!(
        rewound.iter().all(|(_, streaks, _)| *streaks <= 1),
        "no streak runs over the rewound pane: {rewound:?}"
    );
    assert!(
        rewound.iter().any(|(_, streaks, _)| *streaks == 1),
        "CONTROL: the live pane beside it keeps its streak: {rewound:?}"
    );

    assert!(app.rewind_close(0));
    for k in 240..250 {
        let _ = frame(&mut app, k);
    }
    let _ = slice(&app, b"\x1b[2J\x1b[9;3Hwhat the fuck kitty\r\n");
    app.windows
        .get_mut(&WID)
        .expect("window")
        .pending_deco_birth = Some(at(250));
    let again: Vec<_> = (250..370).map(|k| frame(&mut app, k)).collect();
    assert!(
        again.iter().any(|(words, _, _)| *words > 0) && again.iter().any(|(_, s, _)| *s == 2),
        "CONTROL: live again, the pane composes words and runs a streak: {again:?}"
    );
}
