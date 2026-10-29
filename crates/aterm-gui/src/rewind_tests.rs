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

#[test]
fn rewind_insert_pixels_cannot_authorize_current_input() {
    let mut app = scripted_app();
    let session = app.pool.get(0).unwrap();
    let live = session.live_term().clone();
    let context = session.ctx.clone();
    {
        let mut timeline = context.timeline.lock().unwrap();
        timeline.note_foreground_group(300);
        timeline.set_program(300, Some("claude".into()));
    }
    let rule = "─".repeat(30);
    let bytes = format!(
        "\x1b[?1049h\x1b[6;1H{rule}\x1b[7;1H❯ prompt\x1b[8;1H{rule}\x1b[9;1H-- INSERT --\x1b[7;3H"
    );
    term_lock(&live).process(bytes.as_bytes());
    let live_frame = crate::vim_custody::capture(&term_lock(&live), &context, true).unwrap();
    assert_eq!(live_frame.mode, crate::vim_custody::Mode::Insert);
    assert!(app.vim_frame_is_current(WID, live_frame));
    let past = Arc::new(Mutex::new(Terminal::new(24, 80)));
    term_lock(&past).process(bytes.as_bytes());
    let past_frame = crate::vim_custody::capture(&term_lock(&past), &context, true).unwrap();
    for shown in [None, Some(live.clone()), Some(past)] {
        let mut view = RewindView::new(0);
        view.shown = shown;
        app.pool.set_rewind(0, Some(view));
        assert!(!app.vim_frame_is_current(WID, live_frame));
        assert!(!app.vim_frame_is_current(WID, past_frame));
    }
    app.pool.set_rewind(0, None);
    assert!(app.vim_frame_is_current(WID, live_frame));
    assert!(!app.vim_frame_is_current(WID, past_frame));
}

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
    let found = |at, changed: bool, bytes: &[u8]| Found {
        session: 0,
        token: NO_TOKEN,
        cell: (17, 41),
        at,
        change: if changed {
            crate::rewind_worker::Change::Yes
        } else {
            crate::rewind_worker::Change::No
        },
        bytes: bytes.to_vec(),
        resize: None,
        whole: true,
        shown_changed: false,
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
    let far = Found {
        cell: (u16::MAX, u16::MAX),
        ..found(Some(t(1_000_000)), true, b"x")
    };
    assert!(
        changed_note(&far, now).starts_with("row 65536 col 65536 changed "),
        "the 1-based place of the last row and column does not wrap"
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
    start_recording_session(app, 0);
}

fn start_recording_session(app: &App, session: u64) {
    let s = app.pool.get(session).expect("recorded session");
    let (keyframe, pre) = {
        let term = term_lock(s.live_term());
        (
            crate::temporal::keyframe_of(&term, usize::MAX).expect("keyframe"),
            SpinePre::take(&term),
        )
    };
    let mut rec = s.ctx.temporal.lock().expect("recorder");
    rec.record_keyframe(keyframe);
    rec.spine_watermark().seed(pre);
}

/// Feed `bytes` to session 0's engine and, as its reader does, through a spine mark into its recorder, as one slice at a later instant than any before it; that instant.
pub(crate) fn slice(app: &App, bytes: &[u8]) -> Ticks {
    slice_session(app, 0, bytes)
}

fn slice_session(app: &App, session: u64, bytes: &[u8]) -> Ticks {
    slice_clocked(app, session, bytes, ClockReading::now())
}

/// [`slice_session`] with the slice parsed at `clock`.
fn slice_clocked(app: &App, session: u64, bytes: &[u8], clock: ClockReading) -> Ticks {
    let s = app.pool.get(session).expect("recorded session");
    let mut rec = s.ctx.temporal.lock().expect("recorder");
    let last = rec.latest_tick();
    while rec.now() <= last {}
    let (tx, rx) = crate::temporal::queue(8);
    let mut mark = SpineMark::attach(rec.spine_watermark());
    {
        let mut term = term_lock(s.live_term());
        let pre = SpinePre::take(&term);
        term.process_at(bytes, clock);
        let (reports, response) = (term.take_reports(), term.take_response());
        let reply = crate::temporal::reply_of(response.as_deref(), reports.as_deref());
        mark.record(
            &tx,
            pre,
            &term,
            InputSlice::at(bytes, clock).replying(reply),
        );
    }
    while let Some(msg) = rx.try_recv() {
        rec.apply_inline(msg);
    }
    rec.latest_tick()
}

fn advance_recording_clocks(app: &App, sessions: &[u64], ms: u64) {
    for &session in sessions {
        app.pool
            .get(session)
            .unwrap()
            .ctx
            .temporal
            .lock()
            .unwrap()
            .sleep_for_test(std::time::Duration::from_millis(ms));
    }
    crate::turn_ledger::advance_for_test(ms);
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
        around: Vec::new(),
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
    crate::rewind_verb::past_screen_text(&term_lock(&term), view(app).lift)
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

/// The past screen keeps its scrollback: the wheel and Page Up scroll it, at most [`PAST_SCROLLBACK`] lines past those the band lifted, and the live screen does not move.
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
    assert_eq!(
        view(&app).lift,
        2,
        "fixture: the cursor's row was under the band"
    );
    assert_eq!(
        view(&app).scrolled,
        PAST_SCROLLBACK + 2,
        "capped, past the lifted rows"
    );
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
    use crate::rewind_worker::{FORK_EVERY, show_inline, warmed_show};
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
    let show = |at: Option<Ticks>| shown_at(&app, at);
    let screen = |term: &Terminal| {
        let text = crate::control::screen_text(term);
        (text, term.grid().scrollback_lines())
    };
    let mid = Some(ticks[23]);
    let (mut forks, bytes, from_cache) = warmed_show(show(mid));
    forks.sort_unstable();
    assert!(
        forks.len() >= total / (FORK_EVERY + widest),
        "a fork every {FORK_EVERY} bytes: {forks:?}"
    );
    assert!(bytes > 0);
    let straight = show_inline(show(mid));
    assert_eq!(
        screen(&from_cache.term),
        screen(&straight.term),
        "between forks the fold starts from the fork at or before its end"
    );
    assert!(
        from_cache.folded <= FORK_EVERY + widest && straight.folded > FORK_EVERY + widest,
        "from a fork the fold applies at most a fork's spacing and a slice ({} of {})",
        from_cache.folded,
        straight.folded
    );
    assert!(
        screen(&from_cache.term).0.contains("chunk 23 line 49"),
        "negative control: it is the asked instant's screen, not a later fork's"
    );
    let (_, _, newest) = warmed_show(show(None));
    assert_eq!(screen(&newest.term), screen(&show_inline(show(None)).term));
}

/// The fold of session 0's instant `at` (default the spine's end), with the chains a view there warms its forks along.
fn shown_at(app: &App, at: Option<Ticks>) -> crate::rewind_worker::Show {
    let rec = app
        .pool
        .get(0)
        .expect("session 0")
        .ctx
        .temporal
        .lock()
        .expect("recorder");
    let plan = rec.replay_plan(at).expect("a replay");
    let around = rec.warm_plans(at.unwrap_or_else(|| rec.latest_tick()));
    crate::rewind_worker::Show {
        session: 0,
        token: NO_TOKEN,
        plan,
        judge: None,
        around,
        colors: None,
    }
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
    let (kept, _, _) = kept_after(&engine, &seqs, Seq(390));
    assert_eq!(kept.len(), MAX_FORKS);
    assert_eq!(
        kept,
        (8..40).map(|n| Seq(n * 10)).collect::<Vec<_>>(),
        "the forks farthest from the shown instant go first"
    );
    let (kept, _, _) = kept_after(&engine, &seqs, Seq(0));
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
    app.publish_menu_live();
    assert!(
        crate::menu::menu_live().has_selection && app.palette_live().has_selection,
        "Edit > Copy and the palette's Copy are ready for the past selection"
    );
    assert!(app.rewind_close(0));
    app.publish_menu_live();
    assert!(
        !crate::menu::menu_live().has_selection && !app.palette_live().has_selection,
        "negative control: live again, nothing is selected to copy"
    );
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
    app.rewind_step(0, StepBy::Slice(1), None)
        .expect("a step on");
    let stepped = app.find_cmd(FindAction::Status).expect("front");
    assert_eq!(
        (stepped.matches, stepped.current_match),
        (0, None),
        "a step's fold runs find again: no needle at the next instant"
    );
    app.rewind_step(0, StepBy::Slice(-1), None)
        .expect("a step back");
    let back = app.find_cmd(FindAction::Status).expect("front");
    assert_eq!(back.matches, 2, "the needles again");
    let (row, ..) = back.current_match.expect("a current match");
    assert!(
        shown_text(&app)
            .lines()
            .nth(usize::try_from(row).expect("on screen"))
            .is_some_and(|l| l.starts_with("needle")),
        "the match is on the screen stepped back to"
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
            && other.ends_with(" cell=1,0 changed=yes bytes=7 whole=yes shown_changed=no"),
        "negative control: another cell's change is its own: {other}"
    );
    let untouched = ask(&mut app, Asked::Changed(5, 10)).expect("changed");
    assert!(
        untouched.contains(&format!(" at={} ", oldest.0))
            && untouched.ends_with(" cell=5,10 changed=no bytes=0 whole=yes shown_changed=no"),
        "negative control: a cell nothing wrote never changed: {untouched}"
    );
}

/// The momentum fixture with hosted effects attached on its window and session 0 recording `past`; the instant they attached at.
fn hosted(app: &mut App) -> Instant {
    let t0 = Instant::now();
    app.aos_attach_at(aterm_effects::aos_momentum::MomentumCode::Shipped, t0)
        .expect("attached");
    let _ = crate::aos_repair_tests::type_keys(app, t0, 3);
    start_recording(app);
    let _ = slice(app, b"past");
    t0
}

/// Whether window `WID`'s hosted effects are paused.
fn effects_paused(app: &App) -> bool {
    app.windows[&WID].fx.cursor().momentum.aos_paused()
}

/// Opening Rewind pauses the window's hosted effects and the band says so; closing resumes them, and with nothing attached nothing is paused.
#[test]
fn opening_rewind_pauses_the_windows_hosted_effects_and_closing_resumes_them() {
    let mut app = crate::aos_repair_tests::momentum_app(false);
    let _ = hosted(&mut app);
    assert_eq!(app.rewind_effects_status(WID), Some("effects running"));
    app.rewind_open(0, Target::Latest, None).expect("open");
    assert!(effects_paused(&app), "open pauses them");
    let head = app.windows[&WID]
        .fx
        .cursor()
        .momentum
        .aos_paused_head()
        .cloned();
    assert_eq!(
        app.windows[&WID].rewind_paused, head,
        "Rewind keeps the head it paused at"
    );
    app.redraw_window(WID);
    let (rows, _) = grid(&app);
    let band = text(&drawn(&app)[rows - 1..]);
    assert!(band.contains("effects paused"), "{band:?}");
    assert!(app.rewind_close(0));
    assert!(!effects_paused(&app), "close resumes them");
    assert_eq!(app.rewind_effects_status(WID), Some("effects running"));

    let mut bare = scripted_app();
    assert_eq!(
        bare.rewind_effects_status(WID),
        None,
        "control: none attached"
    );
    start_recording(&bare);
    let _ = slice(&bare, b"past");
    bare.rewind_open(0, Target::Latest, None).expect("open");
    assert_eq!(
        bare.windows[&WID].rewind_paused, None,
        "nothing attached, nothing paused"
    );
    bare.redraw_window(WID);
    let (rows, _) = grid(&bare);
    let band = text(&drawn(&bare)[rows - 1..]);
    assert!(!band.contains("effects"), "{band:?}");
}

/// A pause made while Rewind is open (`aos pause` after a resume) moves the head, and closing Rewind leaves that pause alone.
#[test]
fn closing_rewind_leaves_a_pause_made_since_alone() {
    let mut app = crate::aos_repair_tests::momentum_app(false);
    let t0 = hosted(&mut app);
    app.rewind_open(0, Target::Latest, None).expect("open");
    assert!(effects_paused(&app));
    app.aos_resume_now().expect("resumed");
    let at =
        crate::aos_repair_tests::type_keys(&mut app, t0 + std::time::Duration::from_secs(1), 2);
    let (_, head) = app
        .aos_pause_at(at + std::time::Duration::from_millis(10))
        .expect("paused again");
    assert!(app.rewind_close(0));
    let momentum = &app.windows[&WID].fx.cursor().momentum;
    assert_eq!(
        momentum.aos_paused_head(),
        Some(&head),
        "the later pause is kept"
    );
}

/// The pause follows the window's focused pane: focusing a live sibling resumes, focusing the rewound pane again pauses, and closing one of two rewound panes keeps the pause for the other.
#[test]
fn the_effects_pause_follows_the_focused_pane() {
    let mut app = crate::aos_repair_tests::momentum_app(false);
    let _ = hosted(&mut app);
    let sibling = app.split_active_stub_tab(WID);
    start_recording_session(&app, sibling);
    let _ = slice_session(&app, sibling, b"sibling past");
    assert!(app.focus_pane_in(WID, crate::pane::FocusDir::Left));
    assert_eq!(app.focused_session_id(WID), Some(0));
    app.rewind_open(0, Target::Latest, None).expect("open");
    assert!(effects_paused(&app));
    assert!(app.focus_pane_in(WID, crate::pane::FocusDir::Right));
    assert!(!effects_paused(&app), "a live focused pane runs them");
    assert!(app.focus_pane_in(WID, crate::pane::FocusDir::Left));
    assert!(
        effects_paused(&app),
        "the rewound pane focused again pauses them"
    );
    assert!(app.focus_pane_in(WID, crate::pane::FocusDir::Right));
    app.rewind_open(sibling, Target::Latest, None)
        .expect("open on the sibling");
    assert!(
        effects_paused(&app),
        "a view opened on the focused pane pauses them"
    );
    assert!(app.focus_pane_in(WID, crate::pane::FocusDir::Left));
    assert!(app.rewind_close(0));
    assert!(app.focus_pane_in(WID, crate::pane::FocusDir::Right));
    assert!(
        effects_paused(&app),
        "the sibling is still rewound and focused"
    );
    assert!(app.rewind_close(sibling));
    assert!(!effects_paused(&app));
}

/// Closing a rewound pane, or the tab it is in, ends its view: the effects resume, a reply still waiting is answered with an error, and the read-only notice goes.
#[test]
fn closing_a_rewound_pane_or_tab_resumes_the_effects_and_answers_its_replies() {
    for whole_tab in [false, true] {
        let mut app = crate::aos_repair_tests::momentum_app(false);
        let _ = hosted(&mut app);
        let other = if whole_tab {
            let id = app.next_session_id;
            app.push_stub_tab(WID, crate::stub_session(id));
            app.switch_tab_in(WID, 0);
            id
        } else {
            let id = app.split_active_stub_tab(WID);
            assert!(app.focus_pane_in(WID, crate::pane::FocusDir::Left));
            id
        };
        assert_eq!(app.focused_session_id(WID), Some(0));
        let _held = held_view(&mut app);
        let (tx, rx) = std::sync::mpsc::channel();
        app.rewind_open(0, Target::Latest, Some(tx)).expect("open");
        assert!(effects_paused(&app));
        assert!(
            rx.try_recv().is_err(),
            "fixture: the reply waits on the held fold"
        );
        if whole_tab {
            let _ = app.close_active_tab_in(WID);
        } else {
            let _ = app.close_session(WID, 0);
        }
        assert_eq!(
            app.focused_session_id(WID),
            Some(other),
            "whole_tab={whole_tab}"
        );
        assert!(
            !effects_paused(&app),
            "whole_tab={whole_tab}: the effects resume"
        );
        assert!(
            matches!(rx.try_recv(), Ok(Err(_))),
            "whole_tab={whole_tab}: the waiting reply is answered with an error"
        );
        assert!(app.messages.live_by_key(&read_only_key(0)).is_none());
    }
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
    assert_eq!(slider.min_numeric_value(), Some(1.0));
    assert_eq!(slider.numeric_value_step(), Some(1.0));
    let g = app.grid_a11y_geometry(WID).expect("geometry");
    let (_, pane) = app.rewound_panes(WID)[0];
    let band_rows = if pane.rows >= 4 { 2 } else { 1 };
    assert_eq!(
        slider.bounds(),
        Some(aterm_a11y::model::Rect {
            x0: g.origin_x + pane.col as f64 * g.cell_w,
            y0: g.origin_y + (pane.row + pane.rows - band_rows) as f64 * g.cell_h,
            x1: g.origin_x + (pane.col + pane.cols) as f64 * g.cell_w,
            y1: g.origin_y + (pane.row + pane.rows) as f64 * g.cell_h,
        }),
        "the slider sits on the band's rows"
    );
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
    let value = |app: &mut App| {
        app.redraw_window(WID);
        let tree = app.grid_a11y_tree(WID, None).expect("a tree");
        tree.nodes
            .iter()
            .find(|(id, _)| *id == crate::a11y_tree::REWIND)
            .and_then(|(_, node)| node.numeric_value())
    };
    assert_eq!(value(&mut app), Some(2.0), "the value follows the step");
    app.on_accessibility_action(
        WID,
        ActionRequest {
            context: None,
            action: Action::Increment,
            target_tree: TreeId::ROOT,
            target_node: crate::a11y_tree::REWIND,
            data: None,
        },
    );
    assert_eq!(view(&app).state, (3, 3), "Increment steps on");
    assert_eq!(value(&mut app), Some(3.0));
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

/// The events of `cast` played into a fresh engine its size, as screen text.
fn played(cast: &str) -> String {
    let parsed = aterm_control::asciicast::parse(cast).expect("a cast");
    let mut term = Terminal::new(parsed.header.height, parsed.header.width);
    for ev in &parsed.events {
        if let aterm_control::asciicast::CastEvent::Output { data, .. } = ev {
            term.process(data);
        }
    }
    crate::control::screen_text(&term)
}

/// The current recording's export of session 0.
fn exported(app: &App) -> crate::temporal::Export {
    let s = app.pool.get(0).expect("session 0");
    s.ctx
        .temporal
        .lock()
        .expect("recorder")
        .export()
        .expect("an export")
}

/// An export is a file to share: the shell marks keep their kind but not the session's nonce, and a clipboard write split across slices is gone, while the cast still plays back to the live screen.
#[test]
fn an_export_carries_no_nonce_and_no_clipboard_write() {
    let app = scripted_app();
    start_recording(&app);
    let nonce = "3c".repeat(32);
    let _ = slice(&app, format!("\x1b]133;A;id={nonce}\x07$ ").as_bytes());
    let _ = slice(
        &app,
        format!("\x1b]133;B;id={nonce}\x07ls\r\n\x1b]52;c;c2Vj").as_bytes(),
    );
    let _ = slice(
        &app,
        format!("cmV0\x07\x1b]133;D;0;id={nonce}\x07done\r\n").as_bytes(),
    );
    let export = exported(&app);
    let raw: Vec<u8> = export
        .events
        .iter()
        .filter_map(|(_, ev)| match ev {
            crate::temporal::Exported::Output(slice) => Some(slice.bytes().to_vec()),
            _ => None,
        })
        .flatten()
        .collect();
    assert!(
        String::from_utf8_lossy(&raw).contains(&nonce),
        "the recording itself keeps the nonce"
    );
    let (cast, _) = crate::rewind_verb::cast_of(&export);
    assert!(!cast.contains(&nonce), "{cast}");
    assert!(!cast.contains("c2Vj") && !cast.contains("cmV0"), "{cast}");
    assert!(cast.contains("133;A") && cast.contains("133;D;0"), "{cast}");
    assert_eq!(played(&cast), live_text(&app));
}

/// A character whose bytes the reader cut between two slices reaches the cast whole, so playback shows it and no replacement glyph.
#[test]
fn an_export_rejoins_a_character_split_between_slices() {
    let app = scripted_app();
    start_recording(&app);
    let _ = slice(&app, b"a\xE2");
    let _ = slice(&app, b"\x94\x80b\xF0\x9F");
    let _ = slice(&app, b"\x98\x80c\r\n");
    let (cast, _) = crate::rewind_verb::cast_of(&exported(&app));
    assert!(!cast.contains('\u{FFFD}'), "{cast}");
    assert!(played(&cast).contains("a\u{2500}b\u{1F600}c"), "{cast}");
    assert_eq!(played(&cast), live_text(&app));
}

/// What `term` shows: each row with its colours, the cursor, which screen is up and whether the cursor shows.
fn looks(term: &Terminal) -> (Vec<String>, (u16, u16), bool, bool) {
    let rows = (0..term.rows())
        .map(|r| term.grid().row_ansi_text_screen(r).unwrap_or_default())
        .collect();
    let c = term.cursor();
    (
        rows,
        (c.row, c.col),
        term.modes().alternate_screen,
        term.modes().cursor_visible,
    )
}

/// A recording that began on vim's alternate screen, with a scroll region and a pen set, plays back past vim's exit and past a live keyframe taken under tmux-style output to the live screen, its colours and its cursor: each painted screen carries the modes later output depends on.
#[test]
fn an_exported_cast_restores_the_modes_its_painted_screens_were_in() {
    let app = scripted_app();
    let live = app.pool.get(0).expect("session 0").live_term().clone();
    term_lock(&live).process(
        b"$ vim\r\nshell row\r\n\x1b[31m\x1b[?1049h\x1b[2;4r\x1b[44m\x1b[1;1Hvim top\x1b[4;1Hvim low",
    );
    start_recording(&app);
    let _ = slice(&app, b"\nIN REGION\x1b[?1049lback\r\nplain\r\n");
    let (rows, cols) = {
        let term = term_lock(&live);
        (term.rows(), term.cols())
    };
    let mut stranger = Terminal::new(rows, cols);
    stranger.process(b"not what was on screen");
    witness(&app, &stranger);
    {
        let s = app.pool.get(0).expect("session 0");
        let mut rec = s.ctx.temporal.lock().expect("recorder");
        let fold = rec.replay_plan(None).expect("a replay").fold();
        rec.judge(fold.judged);
    }
    let _ = slice(
        &app,
        b"\x1b[?1049h\x1b[?25l\x1b[1;5r\x1b[6;1H\x1b[42mstatus\x1b[m\x1b[5;1Hpane",
    );
    let _ = slice(&app, b"\r\nscrolled\r\nagain\x1b[6;8Hx");
    let export = exported(&app);
    assert!(
        export
            .events
            .iter()
            .any(|(_, ev)| matches!(ev, crate::temporal::Exported::Screen(_))),
        "a live keyframe stands in for the diverged slice"
    );
    let (cast, _) = crate::rewind_verb::cast_of(&export);
    let parsed = aterm_control::asciicast::parse(&cast).expect("a cast");
    let mut played = Terminal::new(parsed.header.height, parsed.header.width);
    for ev in &parsed.events {
        if let aterm_control::asciicast::CastEvent::Output { data, .. } = ev {
            played.process(data);
        }
    }
    assert_eq!(looks(&played), looks(&term_lock(&live)));
}

#[cfg(unix)]
#[test]
fn an_export_is_written_private_and_never_over_a_file() {
    use std::os::unix::fs::PermissionsExt;
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let into = dir.path().join("rewind");
    let path = crate::rewind_verb::write_private(dir.path(), "a.cast", b"cast", "rewind export")
        .expect("written");
    let meta = std::fs::metadata(&path).expect("written");
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    assert_eq!(
        std::fs::metadata(&into).expect("dir").permissions().mode() & 0o777,
        0o700
    );
    assert!(
        crate::rewind_verb::write_private(dir.path(), "a.cast", b"other", "rewind export").is_err()
    );
    assert_eq!(std::fs::read(&path).expect("read"), b"cast");
}

/// `rewind export` refuses a linked `rewind/` folder before it changes anything, as `aos export` does, and makes a loose real one private through the opened folder.
#[cfg(unix)]
#[test]
fn an_export_refuses_a_linked_folder_and_leaves_its_target_alone() {
    use std::os::unix::fs::PermissionsExt;
    let mode =
        |p: &std::path::Path| std::fs::metadata(p).expect("stat").permissions().mode() & 0o777;
    let root = aterm_tempfile::tempdir().expect("a scratch state dir");
    let elsewhere = aterm_tempfile::tempdir().expect("a folder of the person's");
    std::fs::set_permissions(elsewhere.path(), std::fs::Permissions::from_mode(0o755))
        .expect("0755");
    std::os::unix::fs::symlink(elsewhere.path(), root.path().join("rewind")).expect("links");
    let refused =
        crate::rewind_verb::write_private(root.path(), "a.cast", b"cast", "rewind export")
            .expect_err("a link");
    assert!(
        refused
            .to_string()
            .contains("is a symbolic link; rewind export writes only into a real directory"),
        "{refused}"
    );
    assert_eq!(
        mode(elsewhere.path()),
        0o755,
        "the link's target keeps its mode"
    );
    assert!(
        std::fs::read_dir(elsewhere.path())
            .expect("lists")
            .next()
            .is_none(),
        "nothing was written through the link"
    );
    let loose = aterm_tempfile::tempdir().expect("a scratch state dir");
    std::fs::create_dir(loose.path().join("rewind")).expect("made");
    std::fs::set_permissions(
        loose.path().join("rewind"),
        std::fs::Permissions::from_mode(0o755),
    )
    .expect("0755");
    let path = crate::rewind_verb::write_private(loose.path(), "a.cast", b"cast", "rewind export")
        .expect("written");
    assert_eq!(mode(&loose.path().join("rewind")), 0o700);
    assert_eq!(mode(&path), 0o600);
}

/// An export or `rewind check` refusal names its cause: a session that never recorded is pointed at the setting, a recording that broke under a burst or keeps nothing replayable is not.
#[test]
fn an_export_refusal_names_its_cause_and_sends_a_recording_session_nowhere() {
    let app = scripted_app();
    let refusal = |app: &App| {
        let s = app.pool.get(0).expect("session 0");
        crate::rewind_verb::export_of(&s.ctx.temporal.lock().expect("recorder"))
            .err()
            .unwrap_or("exported")
    };
    let check_refusal = |app: &App| {
        let s = app.pool.get(0).expect("session 0");
        crate::rewind_verb::unreplayable(&s.ctx.temporal.lock().expect("recorder"))
    };
    assert!(
        refusal(&app).contains("Settings > Terminal > Rewind"),
        "negative control: {}",
        refusal(&app)
    );
    assert_eq!(check_refusal(&app), refusal(&app));
    start_recording(&app);
    let _ = slice(&app, b"recorded\r\n");
    assert_eq!(refusal(&app), "exported");
    {
        let s = app.pool.get(0).expect("session 0");
        let rec = s.ctx.temporal.lock().expect("recorder");
        let (tx, _rx) = crate::temporal::queue(1);
        let mut mark = SpineMark::attach(rec.spine_watermark());
        let mut term = term_lock(s.live_term());
        for bytes in [&b"a burst"[..], b" the queue drops"] {
            let pre = SpinePre::take(&term);
            let clock = ClockReading::now();
            term.process_at(bytes, clock);
            mark.record(&tx, pre, &term, InputSlice::at(bytes, clock));
        }
        assert!(
            rec.spine_watermark().break_open(),
            "fixture: a break is open"
        );
    }
    let why = refusal(&app);
    assert!(why.contains("broke under a burst"), "{why}");
    assert!(!why.contains("Settings"), "{why}");
    assert_eq!(check_refusal(&app), why, "rewind check says the same");
}

/// View > Export Pane History… writes the panel's file through `export_pane_history_to`: 0600, replacing the file the panel asked about, the chosen folder's mode untouched; a cancel writes nothing, a failed write says so, and with no panel it says how else to export.
#[cfg(unix)]
#[test]
fn the_menu_export_writes_the_panels_file_and_leaves_its_folder_alone() {
    use std::os::unix::fs::PermissionsExt;
    let mode =
        |p: &std::path::Path| std::fs::metadata(p).expect("stat").permissions().mode() & 0o777;
    let mut app = scripted_app();
    start_recording(&app);
    let _ = slice(&app, b"exported\r\n");
    let export = exported(&app);
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let chosen = dir.path().join("Documents");
    std::fs::create_dir(&chosen).expect("folder");
    std::fs::set_permissions(&chosen, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let path = chosen.join("history.cast");
    std::fs::write(&path, b"the file the panel asked about").expect("an old file");
    app.export_pane_history_to(&export, crate::rewind_verb::Chosen::Path(path.clone()));
    assert_eq!(mode(&chosen), 0o755, "the person's folder keeps its mode");
    assert_eq!(mode(&path), 0o600);
    let written = std::fs::read_to_string(&path).expect("read");
    assert!(written.starts_with("{\"version\": 2") && written.contains("exported"));
    let cancelled = chosen.join("cancelled.cast");
    app.export_pane_history_to(&export, crate::rewind_verb::Chosen::Cancelled);
    assert!(!cancelled.exists());
    assert_eq!(
        std::fs::read_dir(&chosen).expect("lists").count(),
        1,
        "a cancel writes nothing"
    );
    let notes = |app: &App| {
        app.messages
            .live_rows()
            .map(|live| format!("{:?}", live.msg))
            .collect::<Vec<_>>()
    };
    assert!(
        !notes(&app).iter().any(|n| n.contains("export")),
        "negative control: the cancel said nothing"
    );
    app.export_pane_history_to(
        &export,
        crate::rewind_verb::Chosen::Path(dir.path().join("gone").join("history.cast")),
    );
    let failed = notes(&app);
    assert!(
        failed.iter().any(|n| n.contains("cannot write")),
        "a failed write is said on the glass: {failed:?}"
    );
    app.export_pane_history_to(&export, crate::rewind_verb::Chosen::NoPanel);
    let notes = notes(&app);
    assert!(
        notes
            .iter()
            .any(|n| n.contains("Run `aterm ctl rewind export`")),
        "{notes:?}"
    );
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
    for slept_ms in [0, 40 * 60 * 1000] {
        a_tab_menu_timeline_row_opens_rewind_at_its_instant_after(slept_ms);
    }
}

/// Let the machine sleep `ms`: session 0's recorder clock and the stamps' sleep run on, and the awake clock does not.
fn machine_slept(app: &App, ms: u64) {
    app.pool
        .get(0)
        .expect("session 0")
        .ctx
        .temporal
        .lock()
        .expect("recorder")
        .sleep_for_test(std::time::Duration::from_millis(ms));
    crate::session_log::sleep_for_test(ms);
}

/// Review fix #13 (R1): the row opens at its instant when the machine slept `slept_ms` after it.
fn a_tab_menu_timeline_row_opens_rewind_at_its_instant_after(slept_ms: u64) {
    use crate::session_chrome::TabMenuEntry;
    let mut app = scripted_app();
    app.tab_strip_rows = 1;
    start_recording(&app);
    let before = slice(&app, b"before the cd");
    advance_recording_clocks(&app, &[0], 1000);
    let s = app.pool.get(0).expect("session 0");
    s.ctx
        .timeline
        .lock()
        .expect("timeline")
        .record_hand_row("cwd-change", "cwd=/tmp".into());
    advance_recording_clocks(&app, &[0], 1000);
    let _ = slice(&app, b"\r\nafter the cd");
    machine_slept(&app, slept_ms);
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
        "the screen as it was when the cd happened, {slept_ms} ms asleep since"
    );
    assert!(shown_text(&app).starts_with("before the cd") && !shown_text(&app).contains("after"));
}

/// A delayed timeline row belongs to the recorded pane whose menu displayed it,
/// even when the same tab survives with a different focused terminal.
#[cfg(unix)]
#[test]
fn a_tab_menu_rewind_row_does_not_retarget_a_sibling_after_focus_or_close() {
    use crate::session_chrome::TabMenuEntry;

    let mut violations = Vec::new();
    for close_source in [false, true] {
        let mut app = scripted_app();
        app.tab_strip_rows = 1;
        let source = 0;
        let sibling = app.split_active_stub_tab(WID);
        assert!(app.focus_pane_in(WID, crate::pane::FocusDir::Left));
        let tab = app.windows[&WID].tab_set.active_id().unwrap();
        let source_view = app.windows[&WID].tab_set.active().unwrap().focus;
        assert_eq!(app.focused_session_id(WID), Some(source));
        for session in [source, sibling] {
            start_recording_session(&app, session);
        }
        let before_source = slice_session(&app, source, b"source before the event");
        let before_sibling = slice_session(&app, sibling, b"sibling before the event");
        advance_recording_clocks(&app, &[source, sibling], 1000);
        app.pool
            .get(source)
            .unwrap()
            .ctx
            .timeline
            .lock()
            .unwrap()
            .record_hand_row("cwd-change", "cwd=/tmp/source".into());
        advance_recording_clocks(&app, &[source, sibling], 1000);
        let after_source = slice_session(&app, source, b"\x1b[2J\x1b[Hsource after the event");
        let after_sibling = slice_session(&app, sibling, b"\x1b[2J\x1b[Hsibling after the event");
        assert!(app.open_active_tab_context_menu(WID));
        let menu = app.windows[&WID].tab_menu.as_ref().unwrap();
        assert_eq!(menu.tab, tab);
        let (origin, at_ms) = menu
            .entries
            .iter()
            .find_map(|entry| match entry {
                TabMenuEntry::RewindTo {
                    words,
                    at_ms,
                    origin,
                } if words.starts_with("changed directory") => {
                    Some((origin.expect("the source row is bound to its view"), *at_ms))
                }
                _ => None,
            })
            .expect("source's actual composed timeline row");
        assert_eq!(origin.view, source_view);
        assert_eq!(origin.session, source);

        // The old dispatcher must have a usable sibling recording to expose the
        // wrong target. Prove both real histories fold to their distinct before
        // screens at the menu's wall-to-recorder target, before changing focus.
        for (session, before, after, prefix) in [
            (source, before_source, after_source, "source before"),
            (sibling, before_sibling, after_sibling, "sibling before"),
        ] {
            let rec = app.pool.get(session).unwrap().ctx.temporal.lock().unwrap();
            let back_us = crate::turn_ledger::now_ms()
                .saturating_sub(at_ms)
                .saturating_mul(1000);
            let target = Ticks(rec.now().0.saturating_sub(back_us));
            assert!(
                before < target && target < after,
                "recording {session}: target {target:?} lies between {before:?} and {after:?}"
            );
            let plan = rec
                .replay_plan(Some(target))
                .expect("both target instants replay");
            drop(rec);
            let folded = crate::rewind_worker::show_inline(crate::rewind_worker::Show {
                session,
                token: NO_TOKEN,
                plan,
                judge: None,
                around: Vec::new(),
                colors: None,
            });
            let text = crate::control::screen_text(&folded.term);
            assert!(
                text.starts_with(prefix) && !text.contains("after"),
                "recording {session}: {text:?}"
            );
            assert!(
                app.pool.rewind(session).is_none(),
                "history preflight opens no review"
            );
        }
        assert!(app.close_tab_menu(WID));
        if close_source {
            assert_eq!(app.close_active_tab_in(WID), None);
            assert!(!app.view_store.contains(source_view));
            assert!(app.pool.get(source).is_none());
        } else {
            assert!(app.focus_pane_in(WID, crate::pane::FocusDir::Right));
        }
        assert_eq!(app.windows[&WID].tab_set.active_id(), Some(tab));
        assert_eq!(app.focused_session_id(WID), Some(sibling));
        let sibling_state = |app: &App| {
            let session = app.pool.get(sibling).unwrap();
            let terminal = term_lock(session.live_term());
            (
                terminal.visible_content(),
                terminal.content_seq(),
                session.ctx.sink.input_epoch(),
            )
        };
        let live_before = sibling_state(&app);
        let root_before = app.windows[&WID].tab_set.active().unwrap().root.clone();
        let config = app.native_config_service.snapshot();

        app.dispatch_tab_menu_rewind(WID, tab, origin, at_ms);

        if app.pool.rewind(sibling).is_some() {
            violations.push(format!(
                "close_source={close_source}: source timeline opened the sibling's Rewind"
            ));
        }
        if close_source {
            if app.focused_session_id(WID) != Some(sibling) || app.pool.rewind(source).is_some() {
                violations.push(
                    "closed source changed the surviving input target or opened a review".into(),
                );
            }
        } else {
            let source_opened = app.pool.rewind(source).is_some_and(|view| {
                view.at == before_source
                    && view.shown.as_ref().is_some_and(|term| {
                        crate::control::screen_text(&term_lock(term)).starts_with("source before")
                    })
            });
            if !source_opened || app.windows[&WID].tab_set.active().unwrap().focus != source_view {
                violations.push(
                    "retained source did not regain its exact view and recorded event".into(),
                );
            }
        }
        assert_eq!(
            sibling_state(&app),
            live_before,
            "live sibling bytes and input stay untouched"
        );
        assert_eq!(
            app.windows[&WID].tab_set.active().unwrap().root,
            root_before
        );
        assert_eq!(app.frontmost_window, Some(WID));
        assert_eq!(app.native_config_service.snapshot(), config);
        assert!(app.structural_invariants_ok());
    }
    assert!(violations.is_empty(), "{}", violations.join("\n"));
}

/// Per-tab binding must not leak one shared session's view identity through its
/// cache, and a surviving mirror cannot replace a closed menu source.
#[cfg(unix)]
#[test]
fn a_tab_menu_rewind_owner_binds_each_shared_view_and_refuses_a_closed_original() {
    use crate::session_chrome::{TabMenuEntry, TerminalMenuOrigin};

    let mut app = scripted_app();
    app.tab_strip_rows = 1;
    start_recording(&app);
    let _ = slice(&app, b"source before the event");
    advance_recording_clocks(&app, &[0], 1000);
    app.pool
        .get(0)
        .unwrap()
        .ctx
        .timeline
        .lock()
        .unwrap()
        .record_hand_row("cwd-change", "cwd=/tmp/source".into());
    advance_recording_clocks(&app, &[0], 1000);
    let _ = slice(&app, b"\x1b[2J\x1b[Hsource after the event");
    let source_tab = app.windows[&WID].tab_set.active_id().unwrap();
    let source_view = app.windows[&WID].tab_set.active().unwrap().focus;
    let mirror = app.open_active_session_in_new_window_logical().unwrap();
    app.window_focus_event(WID, false);
    app.window_focus_event(mirror, true);
    let mirror_view = app.windows[&mirror].tab_set.active().unwrap().focus;
    assert_ne!(source_view, mirror_view);
    assert_eq!(app.pool.views(0), Some(2));
    let row = |entries: &[TabMenuEntry]| {
        entries
            .iter()
            .find_map(|entry| match entry {
                TabMenuEntry::RewindTo {
                    words,
                    at_ms,
                    origin,
                } if words.starts_with("changed directory") => {
                    Some((origin.expect("per-tab menu row is bound"), *at_ms))
                }
                _ => None,
            })
            .expect("the recorded session's timeline row")
    };
    let source_titles = app.tab_titles(WID);
    let source_ext = app.tab_chrome_ext(WID, &source_titles);
    let (origin, at_ms) = row(&source_ext[0].menu);
    assert_eq!(
        origin,
        TerminalMenuOrigin {
            view: source_view,
            session: 0
        }
    );
    let mirror_titles = app.tab_titles(mirror);
    let mirror_ext = app.tab_chrome_ext(mirror, &mirror_titles);
    assert_eq!(
        row(&mirror_ext[0].menu),
        (
            TerminalMenuOrigin {
                view: mirror_view,
                session: 0
            },
            at_ms
        )
    );
    let cached_owners = app.session_chrome[&0]
        .ext
        .menu
        .iter()
        .filter_map(|entry| match entry {
            TabMenuEntry::RewindTo { origin, .. } => Some(*origin),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        !cached_owners.is_empty(),
        "the shared cache contains actual Rewind rows"
    );
    assert!(
        cached_owners.iter().all(Option::is_none),
        "the shared cache stores no view owner"
    );

    let sibling = app.split_active_stub_tab(WID);
    assert!(app.focus_pane_in(WID, crate::pane::FocusDir::Left));
    assert_eq!(app.close_active_tab_in(WID), None);
    assert!(!app.view_store.contains(source_view));
    assert!(app.view_store.contains(mirror_view));
    assert_eq!(
        app.pool.views(0),
        Some(1),
        "the original session remains alive in its mirror"
    );
    assert_eq!(app.focused_session_id(WID), Some(sibling));
    assert_eq!(app.frontmost_window, Some(mirror));
    {
        let recorder = app.pool.get(0).unwrap().ctx.temporal.lock().unwrap();
        let back_us = crate::turn_ledger::now_ms()
            .saturating_sub(at_ms)
            .saturating_mul(1000);
        let target = Ticks(recorder.now().0.saturating_sub(back_us));
        assert!(
            recorder.replay_plan(Some(target)).is_some(),
            "the surviving mirror still has usable source history"
        );
    }
    assert!(app.begin_active_session_rename(mirror));
    assert!(app.rename_field_edit(
        mirror,
        crate::app_search::SearchEdit::Insert("mirror draft".into())
    ));
    let now = std::time::Instant::now();
    for window in app.windows.values_mut() {
        window.fx.cursor_mut().glow.note_typed_cells(now, 1);
        window.fx.cursor_mut().trail.note_typed(now);
    }
    let windows = |app: &App| {
        app.windows
            .iter()
            .map(|(id, window)| {
                (
                    *id,
                    (
                        window.tab_set.clone(),
                        window.layouts.clone(),
                        window.front_content,
                        window.rename_edit.clone(),
                        window.fx.cursor().glow.move_licensed(now),
                        window.fx.cursor().trail.move_licensed(now),
                    ),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let sessions = |app: &App| {
        app.pool
            .iter()
            .map(|session| {
                let terminal = term_lock(session.live_term());
                (
                    session.id,
                    (
                        app.pool.views(session.id),
                        terminal.visible_content(),
                        terminal.content_seq(),
                        session.ctx.sink.input_epoch(),
                        app.pool.rewind(session.id).is_some(),
                    ),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let before_windows = windows(&app);
    let before_sessions = sessions(&app);
    let config = app.native_config_service.snapshot();

    app.dispatch_tab_menu_rewind(WID, source_tab, origin, at_ms);

    assert_eq!(
        windows(&app),
        before_windows,
        "stale delivery preserves both panes, drafts and licences"
    );
    assert_eq!(
        sessions(&app),
        before_sessions,
        "stale delivery cannot rewind the still-live shared session"
    );
    assert_eq!(app.frontmost_window, Some(mirror));
    assert_eq!(
        app.active_handle
            .lock()
            .unwrap()
            .as_ref()
            .map(|session| session.id),
        Some(0)
    );
    assert_eq!(app.native_config_service.snapshot(), config);
    assert!(app.structural_invariants_ok());
}

/// A native sibling becoming focused must not hide a retained terminal menu's
/// owner or give that delayed row authority over another window.
#[cfg(unix)]
#[test]
fn a_tab_menu_rewind_row_refocuses_its_terminal_beside_a_native_view() {
    use crate::native_app::{ActionInvocation, AppEvent, AppViewState, TextInputEvent};
    use crate::session_chrome::TabMenuEntry;

    let mut app = scripted_app();
    app.tab_strip_rows = 1;
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (instance, native) = app.active_native_view(WID).unwrap();
    app.dispatch_native_event(
        WID,
        AppEvent::Action(ActionInvocation {
            id: crate::native_ui::ActionId::new("settings/search"),
            value: None,
        }),
    )
    .unwrap();
    app.dispatch_native_event(
        WID,
        AppEvent::TextInput(TextInputEvent::Commit("native λ draft".into())),
    )
    .unwrap();
    let (source, source_view) =
        app.split_active_with_stub_terminal(WID, crate::tab_model::SplitAxis::Horizontal);
    let tab = app.windows[&WID].tab_set.active_id().unwrap();
    assert_eq!(app.focused_session_id(WID), Some(source));
    start_recording_session(&app, source);
    let before = slice_session(&app, source, b"mixed source before the event");
    advance_recording_clocks(&app, &[source], 1000);
    app.pool
        .get(source)
        .unwrap()
        .ctx
        .timeline
        .lock()
        .unwrap()
        .record_hand_row("cwd-change", "cwd=/tmp/mixed-source".into());
    advance_recording_clocks(&app, &[source], 1000);
    let _ = slice_session(&app, source, b"\x1b[2J\x1b[Hmixed source after the event");
    assert!(app.open_active_tab_context_menu(WID));
    let (origin, at_ms) = app.windows[&WID]
        .tab_menu
        .as_ref()
        .unwrap()
        .entries
        .iter()
        .find_map(|entry| match entry {
            TabMenuEntry::RewindTo {
                words,
                at_ms,
                origin,
            } if words.starts_with("changed directory") => {
                Some((origin.expect("the terminal row is bound"), *at_ms))
            }
            _ => None,
        })
        .expect("the mixed tab's actual terminal timeline row");
    assert_eq!(origin.view, source_view);
    assert_eq!(origin.session, source);
    assert!(app.close_tab_menu(WID));
    assert!(app.focus_pane_in(WID, crate::pane::FocusDir::Left));
    assert_eq!(app.active_native_view(WID), Some((instance, native)));

    let front_session = app.next_session_id;
    let front = app.insert_logical_window(crate::stub_session(front_session), 24, 80);
    term_lock(app.pool.get(front_session).unwrap().live_term()).process(b"unrelated front window");
    app.window_focus_event(WID, false);
    app.window_focus_event(front, true);
    let native_state = |app: &App| {
        let Some(AppViewState::Settings(state)) = app.native_runtime.view_state(native) else {
            panic!("the retained native Settings view");
        };
        (
            state.route,
            state.search.clone(),
            state.common.last_focus.clone(),
            state.common.focus_visible,
            state.page_scroll,
            state.carried_field_drafts(),
        )
    };
    let live_state = |app: &App, session| {
        let session = app.pool.get(session).unwrap();
        let terminal = term_lock(session.live_term());
        (
            terminal.visible_content(),
            terminal.content_seq(),
            session.ctx.sink.input_epoch(),
        )
    };
    let native_before = native_state(&app);
    assert_eq!(native_before.1, "native λ draft");
    let native_binding = app.view_store.get(native).copied();
    let native_generation = app.native_runtime.view_generation(native);
    let root = app.windows[&WID].tab_set.get(tab).unwrap().root.clone();
    let front_tabs = app.windows[&front].tab_set.clone();
    let front_content = app.windows[&front].front_content;
    let front_live = live_state(&app, front_session);
    let source_live = live_state(&app, source);
    let config = app.native_config_service.snapshot();

    app.dispatch_tab_menu_rewind(WID, tab, origin, at_ms);

    assert_eq!(app.windows[&WID].tab_set.active_id(), Some(tab));
    assert_eq!(
        app.windows[&WID].tab_set.active().unwrap().focus,
        source_view
    );
    assert_eq!(app.focused_session_id(WID), Some(source));
    let review = app
        .pool
        .rewind(source)
        .expect("the captured terminal opens Rewind");
    assert_eq!(review.at, before);
    let shown = crate::control::screen_text(&term_lock(review.shown.as_ref().unwrap()));
    assert!(shown.starts_with("mixed source before") && !shown.contains("after"));
    assert_eq!(app.windows[&WID].tab_set.get(tab).unwrap().root, root);
    assert_eq!(native_state(&app), native_before);
    assert_eq!(app.view_store.get(native).copied(), native_binding);
    assert_eq!(
        app.native_runtime.view_generation(native),
        native_generation
    );
    assert!(app.native_runtime.app(instance).is_some());
    assert_eq!(live_state(&app, source), source_live);
    assert_eq!(app.frontmost_window, Some(front));
    assert_eq!(app.windows[&front].tab_set, front_tabs);
    assert_eq!(app.windows[&front].front_content, front_content);
    assert_eq!(live_state(&app, front_session), front_live);
    assert!(app.pool.rewind(front_session).is_none());
    assert_eq!(
        app.active_handle
            .lock()
            .unwrap()
            .as_ref()
            .map(|session| session.id),
        Some(front_session)
    );
    assert_eq!(app.native_config_service.snapshot(), config);
    assert!(app.structural_invariants_ok());
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

/// The band's and the track's rows are chrome: the past screen is lifted so its cursor is drawn above them, a cursor scrolled onto them is not drawn over the band, and a pointer there is over chrome.
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
        view(&app).lift,
        2,
        "fixture: the past cursor was on the band's row"
    );
    assert_eq!(
        usize::from(term_lock(&shown).cursor().row),
        rows - 3,
        "the lift puts the past cursor above the track"
    );
    app.redraw_window(WID);
    assert!(
        app.windows[&WID].input_scratch.cursor_visible,
        "the lifted cursor is drawn"
    );
    app.rewind_scroll(0, 2);
    app.redraw_window(WID);
    assert!(
        !app.windows[&WID].input_scratch.cursor_visible,
        "a cursor scrolled onto the band is not drawn over it"
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
        around: Vec::new(),
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
        around: Vec::new(),
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

/// Review fix #11 (R1): a drag held above a rewound pane scrolls the past screen, not the hidden live one.
#[test]
fn a_drag_held_above_a_rewound_pane_scrolls_the_past_screen() {
    let mut app = scripted_app();
    start_recording(&app);
    let lines: String = (0..200).map(|n| format!("line {n}\r\n")).collect();
    let _ = slice(&app, lines.as_bytes());
    app.redraw_window(WID);
    app.rewind_open(0, Target::Latest, None).expect("open");
    app.redraw_window(WID);
    hover(&mut app, 2, 2);
    app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
    app.on_cursor_moved(WID, 20.0, -1.0);
    app.tick_selection_autoscroll(WID);
    assert!(view(&app).scrolled > 0, "the past screen scrolls");
    let live = app.pool.get(0).expect("session 0").live_term().clone();
    assert_eq!(
        term_lock(&live).grid().display_offset(),
        0,
        "the live screen stays at its bottom"
    );
    app.on_mouse_input(WID, ElementState::Released, MouseButton::Left);
}

/// Review fix #12 (R1): with right_click=copy_paste, a right click over a rewound pane copies and takes the past selection and leaves a live one alone.
#[test]
fn a_right_click_copy_over_a_rewound_pane_takes_the_past_selection() {
    use aterm_core::selection::{SelectionSide, SelectionType};
    let mut app = scripted_app();
    app.config.right_click = Some("copy_paste".to_owned());
    start_recording(&app);
    let past = slice(&app, b"\x1b[2J\x1b[Hthe past screen");
    let _ = slice(&app, b"\x1b[2J\x1b[Hthe live screen now");
    let live = app.pool.get(0).expect("session 0").live_term().clone();
    {
        let mut t = term_lock(&live);
        let selection = t.text_selection_mut();
        selection.start_selection(0, 4, SelectionSide::Left, SelectionType::Simple);
        selection.update_selection(0, 7, SelectionSide::Right);
        selection.complete_selection();
        assert_eq!(t.selection_to_string().as_deref(), Some("live"), "fixture");
    }
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
        "fixture"
    );
    crate::control::PBPASTE_STUB.with(|board| *board.borrow_mut() = Some("PROBE-CLIP".into()));
    app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Right);
    app.on_mouse_input(WID, ElementState::Released, MouseButton::Right);
    let copied = crate::control::PBPASTE_STUB.with(|board| board.borrow_mut().take());
    assert_eq!(
        copied.as_deref(),
        Some("past"),
        "the past selection is copied"
    );
    assert_eq!(
        app.window_selection_text(WID).filter(|s| !s.is_empty()),
        None,
        "and taken"
    );
    assert_eq!(
        term_lock(&live).selection_to_string().as_deref(),
        Some("live"),
        "negative control: the live selection is neither copied nor taken"
    );
}

/// Review fix #38 (R1): a paste key over a rewound pane leaves the hidden live screen where the person scrolled it.
#[test]
fn a_paste_over_a_rewound_pane_leaves_the_live_screen_where_it_was_scrolled() {
    let mut app = scripted_app();
    start_recording(&app);
    let lines: String = (0..200).map(|n| format!("line {n}\r\n")).collect();
    let _ = slice(&app, lines.as_bytes());
    let live = app.pool.get(0).expect("session 0").live_term().clone();
    term_lock(&live).scroll_display(5);
    app.rewind_open(0, Target::Latest, None).expect("open");
    crate::control::PBPASTE_STUB.with(|board| *board.borrow_mut() = Some("probe".into()));
    app.dispatch_action(WID, crate::keybinding::Action::Paste);
    crate::control::PBPASTE_STUB.with(|board| *board.borrow_mut() = None);
    assert_eq!(
        term_lock(&live).grid().display_offset(),
        5,
        "the live screen is where it was scrolled"
    );
    assert!(
        app.messages.live_by_key(&read_only_key(0)).is_some(),
        "negative control: the paste was withheld and said so"
    );
}

/// Review fix #39 (R1): a wheel glide in flight over the live screen lands before the view opens, so it neither moves the live screen on nor offsets the past frame.
#[test]
fn a_glide_in_flight_lands_before_the_view_opens() {
    let mut app = scripted_app();
    start_recording(&app);
    let lines: String = (0..200).map(|n| format!("line {n}\r\n")).collect();
    let _ = slice(&app, lines.as_bytes());
    let live = app.pool.get(0).expect("session 0").live_term().clone();
    app.scroll_wheel_animated(WID, &live, 3);
    assert!(
        app.windows[&WID].scroll_glide.is_some(),
        "fixture: a glide in flight"
    );
    app.rewind_open(0, Target::Latest, None).expect("open");
    let ws = &app.windows[&WID];
    assert!(
        ws.scroll_glide.is_none() && ws.overscroll.is_none(),
        "the glide landed"
    );
    assert_eq!(ws.scroll_frac_px, 0, "and offsets nothing");
    assert_eq!(
        term_lock(&live).grid().display_offset(),
        3,
        "at the row it was going to"
    );
}

/// Review fix #34 (R1): while a pane is rewound, its window and its session's store handle keep the live engine's title.
#[test]
fn a_rewound_pane_keeps_the_live_title_on_its_window_and_in_the_store() {
    let mut app = scripted_app();
    start_recording(&app);
    let vim = slice(&app, b"\x1b]2;vim notes.md\x07");
    let _ = slice(&app, b"\x1b]2;zsh\x07");
    let store_title = |app: &App| {
        let id = app.pool.get(0).expect("session 0").id;
        app.store
            .read()
            .expect("store")
            .by_local(id)
            .map(|h| h.title.clone())
    };
    assert_eq!(
        app.publish_active_terminal_title(WID, "zsh"),
        "zsh",
        "fixture: live, the shown title is the window's"
    );
    app.rewind_open(0, Target::At(vim), None).expect("open");
    let shown = term_lock(&view(&app).shown.clone().expect("shown"))
        .title_arc()
        .to_string();
    assert_eq!(
        shown, "vim notes.md",
        "fixture: the past carries its own title"
    );
    assert_eq!(app.publish_active_terminal_title(WID, &shown), "zsh");
    assert_eq!(store_title(&app).as_deref(), Some("zsh"));
    assert!(app.rewind_close(0));
    assert_eq!(
        app.publish_active_terminal_title(WID, "vim notes.md"),
        "vim notes.md",
        "negative control: live again, the shown title is the window's"
    );
}

/// Review fix #35 (R1): a view opened or closed under an open find leaves the live screen as find found it once find ends.
#[test]
fn a_view_opened_or_closed_under_an_open_find_leaves_the_live_screen_as_find_found_it() {
    use crate::app_search::FindAction;
    let live_state = |app: &App| {
        let live = app.pool.get(0).expect("session 0").live_term().clone();
        let t = term_lock(&live);
        (
            t.grid().display_offset(),
            t.selection_to_string().filter(|s| !s.is_empty()),
        )
    };
    let history: String = std::iter::once("needle up here\r\n".to_owned())
        .chain((0..200).map(|n| format!("line {n}\r\n")))
        .collect();

    let mut app = scripted_app();
    start_recording(&app);
    let _ = slice(&app, history.as_bytes());
    app.find_cmd(FindAction::Open).expect("front");
    let found = app
        .find_cmd(FindAction::Type("needle".into()))
        .expect("front");
    assert_eq!(found.matches, 1, "fixture");
    assert_ne!(
        live_state(&app),
        (0, None),
        "fixture: find scrolled the live screen to its match and selected it"
    );
    app.rewind_open(0, Target::Latest, None).expect("open");
    app.find_cmd(FindAction::Cancel);
    assert!(app.rewind_close(0));
    assert_eq!(
        live_state(&app),
        (0, None),
        "find on live, open, Esc, close"
    );

    let mut app = scripted_app();
    start_recording(&app);
    let _ = slice(&app, history.as_bytes());
    app.rewind_open(0, Target::Latest, None).expect("open");
    app.rewind_scroll(0, 20);
    app.find_cmd(FindAction::Open).expect("front");
    let found = app
        .find_cmd(FindAction::Type("needle".into()))
        .expect("front");
    assert_eq!(found.matches, 1, "fixture: the past holds the needle");
    assert!(app.rewind_close(0));
    assert_ne!(
        live_state(&app),
        (0, None),
        "fixture: find runs again on live"
    );
    app.find_cmd(FindAction::Cancel);
    assert_eq!(
        live_state(&app),
        (0, None),
        "open, scroll, find, close, Esc"
    );
}

/// Review fix #36 (R1): a Shift pressed over a rewound pane reports neither its press nor its release to the program, and one pressed while live still reports its release.
#[cfg(unix)]
#[test]
fn a_key_withheld_while_rewound_reports_no_release_and_a_live_press_still_does() {
    use aterm_window::keyboard::{KeyCode, KeyLocation, PhysicalKey};
    let shift = |state| {
        aterm_window::event::KeyEvent::synthetic_for_test(
            PhysicalKey::Code(KeyCode::ShiftLeft),
            WinitKey::Named(NamedKey::Shift),
            None,
            KeyLocation::Left,
            state,
            false,
        )
    };
    let (mut app, mut program) = piped();
    start_recording(&app);
    let _ = slice(&app, b"\x1b[>10u$ ");
    app.on_key(WID, shift(ElementState::Pressed));
    assert_eq!(
        program_read(&mut program),
        b"\x1b[57441;2u",
        "fixture: the program hears every key"
    );
    app.rewind_open(0, Target::Latest, None).expect("open");
    app.on_key(WID, shift(ElementState::Released));
    assert_eq!(
        program_read(&mut program),
        b"\x1b[57441;1:3u",
        "negative control: a press made live reports its release"
    );
    app.on_key(WID, shift(ElementState::Pressed));
    app.on_key(WID, shift(ElementState::Released));
    assert!(
        program_read(&mut program).is_empty(),
        "neither the withheld press nor its release reaches the program"
    );
}

/// Review fix #50 (R1): the track hatches in its label ink the stretch nothing kept rebuilds, draws the kept rest plain and the shown instant at its own column, and a recording that broke is Lost from end to end.
#[test]
fn the_track_hatches_what_nothing_rebuilds_and_marks_the_shown_instant_at_its_column() {
    use crate::rewind_band::{HATCH, KEPT, SHOWN, track_row};
    use crate::temporal::Span;
    let inks = crate::chrome_band::band_colors(aterm_render::Theme::default());
    assert_ne!(
        inks.label, inks.warn,
        "fixture: Lost and Differs have their own inks"
    );
    let mut view = view_at((2, 3), Faith::Unchecked);
    view.track = crate::temporal::Track {
        from: t(0),
        to: t(999),
        spans: vec![(t(0), t(499), Span::Lost)],
    };
    view.at = t(750);
    let row = track_row(&view, 22, &inks);
    for k in 0..20 {
        let want = match k {
            15 => crate::chrome_band::cell(SHOWN, inks.accent, inks.bar_bg, true, false),
            0..=9 => crate::chrome_band::cell(HATCH, inks.label, inks.bar_bg, false, false),
            _ => crate::chrome_band::cell(KEPT, inks.label, inks.bar_bg, false, false),
        };
        assert_eq!(row[1 + k], want, "column {}", 1 + k);
    }

    let app = scripted_app();
    start_recording(&app);
    let _ = slice(&app, b"kept\r\n");
    let track = |app: &App| {
        let s = app.pool.get(0).expect("session 0");
        s.ctx.temporal.lock().expect("recorder").rewind_track()
    };
    assert!(
        !track(&app)
            .spans
            .iter()
            .any(|(.., kind)| *kind == Span::Lost),
        "negative control: all of it replays"
    );
    {
        let s = app.pool.get(0).expect("session 0");
        let rec = s.ctx.temporal.lock().expect("recorder");
        let (tx, _rx) = crate::temporal::queue(1);
        let mut mark = SpineMark::attach(rec.spine_watermark());
        let mut term = term_lock(s.live_term());
        for bytes in [&b"a burst"[..], b" the queue drops"] {
            let pre = SpinePre::take(&term);
            let clock = ClockReading::now();
            term.process_at(bytes, clock);
            mark.record(&tx, pre, &term, InputSlice::at(bytes, clock));
        }
        assert!(
            rec.spine_watermark().break_open(),
            "fixture: a break is open"
        );
    }
    let broke = track(&app);
    assert!(
        broke.spans.contains(&(broke.from, broke.to, Span::Lost)),
        "{:?}",
        broke.spans
    );
}

/// Review fixes #15 and #37 (R1): the band and the track cover no row of the past screen: a screen written to its last row is drawn two rows higher, find's match on that row sits above the band, the top rows are a scroll away, and `rewind text` reads the screen as it was.
#[test]
fn the_band_covers_no_past_row_and_the_lifted_rows_are_a_scroll_away() {
    use crate::app_search::FindAction;
    use crate::rewind_verb::Asked;
    let mut app = scripted_app();
    app.redraw_window(WID);
    start_recording(&app);
    let (rows, _) = grid(&app);
    let lines: Vec<String> = (0..rows)
        .map(|r| match r {
            0 => "TOP".to_owned(),
            1 => "SECOND".to_owned(),
            r if r + 2 == rows => "PENULT".to_owned(),
            r if r + 1 == rows => "BOTTOM".to_owned(),
            r => format!("row {r}"),
        })
        .collect();
    let past = slice(
        &app,
        format!("\x1b[2J\x1b[H{}", lines.join("\r\n")).as_bytes(),
    );
    let _ = slice(&app, b"\x1b[2J\x1b[Hlive");
    app.rewind_open(0, Target::At(past), None).expect("open");
    app.redraw_window(WID);
    let shown = drawn(&app);
    assert_eq!(
        text(&shown[rows - 4..rows - 3]),
        "PENULT",
        "above the track"
    );
    assert_eq!(
        text(&shown[rows - 3..rows - 2]),
        "BOTTOM",
        "and the row under it"
    );
    assert_eq!(text(&shown[..1]), "row 2", "the top two rows are lifted");
    app.find_cmd(FindAction::Open).expect("front");
    let found = app
        .find_cmd(FindAction::Type("BOTTOM".into()))
        .expect("front");
    assert_eq!(found.matches, 1, "fixture");
    let (row, ..) = found.current_match.expect("a current match");
    assert_eq!(
        usize::try_from(row).expect("on the screen"),
        rows - 3,
        "find's match on the past's last row sits above the band"
    );
    app.find_cmd(FindAction::Cancel);
    let reply = ask(&mut app, Asked::Text { trim: true }).expect("text");
    let (_, body) = reply.split_once('\n').expect("a header and a body");
    assert_eq!(
        body,
        format!("{}\n", lines.join("\n")),
        "rewind text reads the screen as it was"
    );
    assert!(app.rewind_scroll(0, 2));
    app.redraw_window(WID);
    let scrolled = drawn(&app);
    assert_eq!(
        text(&scrolled[..1]),
        "TOP",
        "a scroll of two shows the top row"
    );
    assert_eq!(text(&scrolled[1..2]), "SECOND");
    assert!(
        !app.rewind_scroll(0, 1) || view(&app).scrolled > 2,
        "the screen's own scrollback is past the lifted rows"
    );
}

/// The lifted rows stay a scroll away under a session that keeps no scrollback, whose limit the fold takes (finding 46), and `rewind text` still reads them.
#[test]
fn a_session_with_no_scrollback_keeps_its_lifted_rows_a_scroll_away() {
    use crate::rewind_verb::Asked;
    let mut app = scripted_app();
    app.session_factory.terminal_config = Some(aterm_core::config::TerminalConfig {
        scrollback_limit: Some(0),
        ..aterm_core::config::TerminalConfig::default()
    });
    term_lock(&live_term(&app)).set_scrollback_line_limit(Some(0));
    app.redraw_window(WID);
    start_recording(&app);
    let (rows, _) = grid(&app);
    let lines: Vec<String> = (0..rows)
        .map(|r| {
            if r == 0 {
                "TOP".to_owned()
            } else {
                format!("row {r}")
            }
        })
        .collect();
    let past = slice(
        &app,
        format!("\x1b[2J\x1b[H{}", lines.join("\r\n")).as_bytes(),
    );
    let _ = slice(&app, b"\x1b[2J\x1b[Hlive");
    app.rewind_open(0, Target::At(past), None).expect("open");
    assert_eq!(
        view(&app).lift,
        2,
        "fixture: the screen is written to its last row"
    );
    let reply = ask(&mut app, Asked::Text { trim: true }).expect("text");
    let (_, body) = reply.split_once('\n').expect("a header and a body");
    assert_eq!(
        body,
        format!("{}\n", lines.join("\n")),
        "rewind text keeps the top rows"
    );
    assert!(app.rewind_scroll(0, 2));
    app.redraw_window(WID);
    assert_eq!(
        text(&drawn(&app)[..1]),
        "TOP",
        "a scroll of two shows the top row"
    );
    assert!(
        !app.rewind_scroll(0, 1),
        "and nothing past it: the session keeps no scrollback"
    );
}

/// Review fix #22 (R1): a press on the band's or the track's row picks no cell and starts no selection, so B asks for a cell rather than read one hidden under the band.
#[test]
fn a_press_on_the_band_or_the_track_picks_no_cell() {
    let mut app = scripted_app();
    app.redraw_window(WID);
    start_recording(&app);
    let (rows, _) = grid(&app);
    let past = slice(&app, b"\x1b[2J\x1b[Hthe past screen");
    let _ = slice(&app, b"\x1b[2J\x1b[Hlive");
    app.rewind_open(0, Target::At(past), None).expect("open");
    app.redraw_window(WID);
    for row in [rows - 1, rows - 2] {
        hover(&mut app, u16::try_from(row).expect("a row"), 3);
        app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(WID, ElementState::Released, MouseButton::Left);
        assert_eq!(
            view(&app).picked,
            None,
            "row {row}: nothing under the band is picked"
        );
        assert_eq!(
            app.window_selection_text(WID).filter(|s| !s.is_empty()),
            None,
            "row {row}: and nothing is selected"
        );
    }
    assert_eq!(
        app.rewind_changed(0),
        Err("click a cell first, then press B".to_string())
    );
    hover(&mut app, 0, 3);
    app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(WID, ElementState::Released, MouseButton::Left);
    assert_eq!(
        view(&app).picked,
        Some((0, 3)),
        "negative control: a press above the band picks its cell"
    );
}

/// A view whose fold thread hands every result to the test, which delivers each by hand.
fn handed_view(app: &mut App) -> std::sync::mpsc::Receiver<Done> {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut view = RewindView::new(0);
    view.worker =
        Some(RewindWorker::start(move |done| tx.send(done).is_ok()).expect("a fold thread"));
    assert!(
        app.pool.set_rewind(0, Some(view)).is_none(),
        "fixture: no view was open"
    );
    rx
}

/// The next result the handed thread posts (a one-minute hang detector).
fn handed(rx: &std::sync::mpsc::Receiver<Done>) -> Done {
    rx.recv_timeout(std::time::Duration::from_secs(60))
        .expect("the fold thread posts")
}

/// Session 0's live engine.
fn live_term(app: &App) -> Arc<Mutex<Terminal>> {
    app.pool.get(0).expect("session 0").live_term().clone()
}

/// Write `bytes` on session 0's live screen only, as a host change the spine never sees.
fn unrecorded(app: &App, bytes: &[u8]) {
    term_lock(&live_term(app)).process(bytes);
}

/// `rewind changed` on cell `(row, col)` of a view opened at the newest state.
fn changed_at_latest(app: &mut App, (row, col): (u16, u16)) -> String {
    use crate::rewind_verb::Asked;
    let _ = ask(app, Asked::Open(Target::Latest)).expect("open");
    ask(app, Asked::Changed(row, col)).expect("changed")
}

/// B reads a blank past the written part of a row as the blank the screen draws there: a write further right is no change to it, and a new default background is one.
#[test]
fn b_reads_a_blank_tail_cell_as_the_blank_drawn_there() {
    let mut app = scripted_app();
    start_recording(&app);
    let oldest = instants(&app)[0].at;
    let _ = slice(&app, b"$ ls");
    let _ = slice(&app, b"\x1b[1;61Hx");
    let _ = slice(&app, b"\x1b[3;1Hw");
    let tail = changed_at_latest(&mut app, (0, 40));
    assert!(
        tail.contains(&format!(" at={} ", oldest.0)) && tail.contains(" changed=no "),
        "a write at col 61 leaves col 41 blank: {tail}"
    );

    let mut app = scripted_app();
    start_recording(&app);
    let _ = slice(&app, b"$ ls");
    let white = slice(&app, b"\x1b]11;#ffffff\x07");
    let _ = slice(&app, b"\x1b[3;1Hw");
    let tail = changed_at_latest(&mut app, (0, 40));
    assert!(
        tail.contains(&format!(" at={} ", white.0)) && tail.contains(" changed=yes "),
        "a new default background changes every blank tail cell: {tail}"
    );

    let mut app = scripted_app();
    start_recording(&app);
    let _ = slice(&app, b"\x1b[1;6Hx");
    let erased = slice(&app, b"\x1b[2K");
    let _ = slice(&app, b"\x1b[3;1Hw");
    let erase = changed_at_latest(&mut app, (0, 5));
    assert!(
        erase.contains(&format!(" at={} ", erased.0)) && erase.contains(" changed=yes "),
        "negative control: erasing a written cell changes it: {erase}"
    );
}

/// B on the right half of a wide glyph sees the glyph: a rewrite of either half is a change.
#[test]
fn b_on_a_wide_glyphs_right_half_finds_where_the_glyph_changed() {
    use crate::rewind_verb::Asked;
    let mut app = scripted_app();
    start_recording(&app);
    let zhong = slice(&app, "\x1b[1;2H\u{4e2d}".as_bytes());
    let wen = slice(&app, "\x1b[1;2H\u{6587}".as_bytes());
    let _ = slice(&app, b"\x1b[3;1Hw");
    let right = changed_at_latest(&mut app, (0, 2));
    assert!(
        right.contains(&format!(" at={} ", wen.0)) && right.contains(" changed=yes "),
        "the glyph's rewrite: {right}"
    );
    let again = ask(&mut app, Asked::Changed(0, 2)).expect("changed");
    assert!(
        again.contains(&format!(" at={} ", zhong.0)) && again.contains(" changed=yes "),
        "B again: the glyph's first write: {again}"
    );
}

/// B compares a cell's whole grapheme: a mark or a sequence tail rewritten over the same base is a change, and the same grapheme written again is not.
#[test]
fn b_finds_a_change_to_a_cells_marks_or_sequence_tail() {
    for (before, after) in [
        ("e", "e\u{301}"),
        ("e\u{301}", "e\u{300}"),
        ("\u{1f44d}", "\u{1f44d}\u{1f3fd}"),
        ("\u{1f1fa}\u{1f1f8}", "\u{1f1fa}\u{1f1e6}"),
    ] {
        let mut app = scripted_app();
        start_recording(&app);
        let _ = slice(&app, format!("\x1b[1;1H{before}").as_bytes());
        let later = slice(&app, format!("\x1b[1;1H{after}").as_bytes());
        let _ = slice(&app, b"\x1b[3;1Hw");
        let found = changed_at_latest(&mut app, (0, 0));
        assert!(
            found.contains(&format!(" at={} ", later.0)) && found.contains(" changed=yes "),
            "{before:?} then {after:?}: {found}"
        );
    }
    let mut app = scripted_app();
    start_recording(&app);
    let first = slice(&app, "\x1b[1;1He\u{301}".as_bytes());
    let _ = slice(&app, "\x1b[1;1He\u{301}".as_bytes());
    let _ = slice(&app, b"\x1b[3;1Hw");
    let same = changed_at_latest(&mut app, (0, 0));
    assert!(
        same.contains(&format!(" at={} ", first.0)),
        "negative control: the same grapheme again is no change: {same}"
    );
}

/// A cell search does not trust a replay a live witness contradicts: past the live keyframe that replaced a diverged chain it says the history before is unknown, and credits no later slice with the keyframe's screen.
#[test]
fn b_stops_at_a_diverged_chain_and_credits_no_slice_with_its_mistake() {
    use crate::rewind_verb::Asked;
    let mut app = scripted_app();
    start_recording(&app);
    let _ = slice(&app, b"recorded");
    unrecorded(&app, b"\x1b[5;1Hunrecorded");
    witness(&app, &term_lock(&live_term(&app)));
    let _ = ask(&mut app, Asked::Open(Target::Latest)).expect("open");
    assert!(
        app.pool
            .get(0)
            .expect("session 0")
            .ctx
            .temporal
            .lock()
            .expect("recorder")
            .spine_watermark()
            .keyframe_wanted(),
        "fixture: the witness diverged and the reader owes a live keyframe"
    );
    let keyframe = slice(&app, b"\x1b[2;1Hafter");
    let one = slice(&app, b"\x1b[3;1Hone");
    let _ = slice(&app, b"\x1b[6;1Htwo");
    assert_eq!(
        app.pool
            .get(0)
            .expect("session 0")
            .ctx
            .temporal
            .lock()
            .expect("recorder")
            .keyframe_count(),
        2,
        "fixture: two chains"
    );
    let made_up = changed_at_latest(&mut app, (4, 0));
    assert!(
        made_up.contains(&format!(" at={} ", keyframe.0)) && made_up.contains(" changed=unknown "),
        "the history before the live keyframe is unknown: {made_up}"
    );
    let own = changed_at_latest(&mut app, (2, 0));
    assert!(
        own.contains(&format!(" at={} ", one.0)) && own.contains(" changed=yes "),
        "negative control: a change inside the trusted chain is found: {own}"
    );
}

/// When the shown state itself changed the cell, B answers that state; B again goes on past it, and says so.
#[test]
fn b_answers_the_shown_state_when_it_changed_the_cell_and_b_again_the_one_before() {
    use crate::rewind_verb::Asked;
    let mut app = scripted_app();
    start_recording(&app);
    let oldest = instants(&app)[0].at;
    let _ = slice(&app, b"$ ls");
    let w = slice(&app, b"\x1b[3;1Hw");
    let own = changed_at_latest(&mut app, (2, 0));
    assert!(
        own.contains(&format!(" at={} ", w.0))
            && own.contains(" changed=yes bytes=7 ")
            && own.ends_with(" shown_changed=no"),
        "the shown state wrote the cell: {own}"
    );
    let again = ask(&mut app, Asked::Changed(2, 0)).expect("changed");
    assert!(
        again.contains(&format!(" at={} ", oldest.0))
            && again.contains(" changed=no ")
            && again.ends_with(" shown_changed=yes"),
        "B again: before that the cell was as at the oldest state: {again}"
    );
    let untouched = changed_at_latest(&mut app, (5, 10));
    assert!(
        untouched.contains(&format!(" at={} ", oldest.0))
            && untouched.ends_with(" shown_changed=no"),
        "negative control: a cell nothing wrote: {untouched}"
    );
}

/// A cell off the past screen is refused in words and the view stays, the largest row and column included.
#[test]
fn a_cell_off_the_past_screen_is_refused_and_the_view_stays() {
    use crate::rewind_verb::Asked;
    let mut app = scripted_app();
    start_recording(&app);
    let _ = slice(&app, b"$ ls");
    let last = slice(&app, b"\r\n$ ");
    let _ = ask(&mut app, Asked::Open(Target::Latest)).expect("open");
    let (rows, cols) = {
        let shown = view(&app).shown.clone().expect("shown");
        let t = term_lock(&shown);
        (t.rows(), t.cols())
    };
    for (row, col) in [
        (u16::MAX, 0),
        (0, u16::MAX),
        (rows, 0),
        (0, cols),
        (500, 500),
    ] {
        let why = ask(&mut app, Asked::Changed(row, col)).expect_err("refused");
        assert!(
            why.starts_with("that cell is off the past screen"),
            "{row},{col}: {why}"
        );
        assert_eq!(view(&app).at, last, "{row},{col}: the view stays");
    }
    let edge = ask(&mut app, Asked::Changed(rows - 1, cols - 1));
    assert!(
        edge.is_ok(),
        "negative control: the last cell walks: {edge:?}"
    );
}

/// A step, Home or a later open taken while a cell search is out gives the search up: its reply says so, and its late result moves the view nowhere.
#[test]
fn a_step_or_open_during_a_cell_search_supersedes_it_and_its_late_result_moves_nothing() {
    for open in [false, true] {
        let mut app = scripted_app();
        start_recording(&app);
        let oldest = instants(&app)[0].at;
        let x = slice(&app, b"\x1b[1;1Hx");
        let _ = slice(&app, b"\x1b[2;1Hy");
        let rx = handed_view(&mut app);
        app.rewind_open(0, Target::Latest, None).expect("open");
        let shown = handed(&rx);
        app.rewind_done(shown);
        let (tx, reply) = std::sync::mpsc::channel();
        app.rewind_changed_for(0, Some((0, 0)), Some(tx))
            .expect("search");
        let found = handed(&rx);
        assert!(
            matches!(&found, Done::Changed(f) if f.at == Some(x)),
            "fixture: the search found x: {found:?}"
        );
        let why = if open {
            app.rewind_open(0, Target::At(oldest), None)
                .expect("open again");
            "a later rewind open replaced that cell search"
        } else {
            app.rewind_step(0, StepBy::End(false), None).expect("Home");
            "a step replaced that cell search"
        };
        assert_eq!(reply.try_recv(), Ok(Err(why.to_string())), "open={open}");
        app.rewind_done(found);
        let moved = handed(&rx);
        app.rewind_done(moved);
        let v = view(&app);
        assert_eq!(
            v.at, oldest,
            "the view is where the last ask put it (open={open})"
        );
        assert!(
            v.folding.is_none() && v.queued.is_none() && v.note.is_none(),
            "the late result asked for nothing and says nothing (open={open})"
        );
    }
}

/// `rewind status` says the view's thread is busy while a fold is out, and idle once the thread has nothing left in hand.
#[test]
fn rewind_status_says_the_worker_is_busy_until_its_work_is_done() {
    use crate::rewind_verb::Asked;
    let mut app = scripted_app();
    start_recording(&app);
    let _ = slice(&app, b"\x1b[1;1Hx");
    let rx = handed_view(&mut app);
    app.rewind_open(0, Target::Latest, None).expect("open");
    let status = ask(&mut app, Asked::Status).expect("status");
    assert!(status.ends_with(" worker=busy"), "a fold is out: {status}");
    let shown = handed(&rx);
    app.rewind_done(shown);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let status = ask(&mut app, Asked::Status).expect("status");
        if status.ends_with(" worker=idle") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the thread never settled: {status}"
        );
        std::thread::yield_now();
    }
}

/// A second cell search answers the first one's reply in words, and its own reply answers once its state is shown.
#[test]
fn a_second_cell_search_answers_the_first_and_its_own_once_shown() {
    let mut app = scripted_app();
    start_recording(&app);
    let _ = slice(&app, b"\x1b[1;1Hx");
    let y = slice(&app, b"\x1b[2;1Hy");
    let _ = slice(&app, b"\x1b[3;1Hw");
    let rx = handed_view(&mut app);
    app.rewind_open(0, Target::Latest, None).expect("open");
    let shown = handed(&rx);
    app.rewind_done(shown);
    let (tx1, first_reply) = std::sync::mpsc::channel();
    app.rewind_changed_for(0, Some((0, 0)), Some(tx1))
        .expect("first search");
    let first = handed(&rx);
    let (tx2, second_reply) = std::sync::mpsc::channel();
    app.rewind_changed_for(0, Some((1, 0)), Some(tx2))
        .expect("second search");
    assert_eq!(
        first_reply.try_recv(),
        Ok(Err(
            "a newer cell search on this view replaced that one".to_string()
        ))
    );
    app.rewind_done(first);
    assert!(
        view(&app).folding.is_none(),
        "the first search's late result moves nothing"
    );
    let second = handed(&rx);
    app.rewind_done(second);
    let shown = handed(&rx);
    app.rewind_done(shown);
    let line = second_reply
        .try_recv()
        .expect("the second search answers")
        .expect("found");
    assert!(
        line.contains(&format!(" at={} ", y.0)) && line.contains(" cell=1,0 changed=yes "),
        "{line}"
    );
}

/// Opening the view on another state, from the verb or a tab-menu row, drops the last cell search's note.
#[test]
fn opening_another_state_drops_the_last_cell_searchs_note() {
    use crate::rewind_verb::Asked;
    let mut app = scripted_app();
    start_recording(&app);
    let _ = slice(&app, b"\x1b[1;1Hx");
    let _ = changed_at_latest(&mut app, (0, 0));
    assert!(
        view(&app).note.is_some(),
        "fixture: the search noted its find"
    );
    let _ = ask(&mut app, Asked::Open(Target::Latest)).expect("open");
    assert_eq!(view(&app).note, None);
}

/// A slice on the tick of the live keyframe after it is read with the newer chain, and its bytes go with the change it made.
#[test]
fn a_slice_on_a_live_keyframes_tick_brings_its_bytes_to_the_change() {
    let mut app = scripted_app();
    start_recording(&app);
    let _ = slice(&app, b"$ ");
    let tie = {
        let s = app.pool.get(0).expect("session 0");
        let mut rec = s.ctx.temporal.lock().expect("recorder");
        let last = rec.latest_tick();
        while rec.now() <= last {}
        let tie = rec.now();
        rec.freeze_now_for_test(Some(tie));
        let live = s.live_term().clone();
        let mut term = term_lock(&live);
        let clock = ClockReading::now();
        term.process_at(b"\x1b[1;1Hx", clock);
        rec.apply_inline(TemporalMsg::RawIn(InputSlice::at(b"\x1b[1;1Hx", clock)));
        rec.apply_inline(TemporalMsg::LiveKeyframe(
            Arc::new(term.checkpoint()).into(),
            crate::temporal::SliceMarks::default(),
        ));
        rec.freeze_now_for_test(None);
        tie
    };
    let _ = slice(&app, b"\x1b[3;1Hw");
    let found = changed_at_latest(&mut app, (0, 0));
    assert!(
        found.contains(&format!(" at={} ", tie.0))
            && found.contains(" changed=yes bytes=7 whole=no "),
        "the slice's bytes, and word that the keyframe's own were not kept: {found}"
    );
}

/// B walks back across a keyframe into the chain before it.
#[test]
fn b_walks_back_across_a_keyframe_into_the_older_chain() {
    let mut app = scripted_app();
    start_recording(&app);
    let x = slice(&app, b"\x1b[1;1Hx");
    let _ = slice(&app, b"\x1b[2;1Hy");
    start_recording(&app);
    let _ = slice(&app, b"\x1b[3;1Hz");
    let _ = slice(&app, b"\x1b[4;1Hw");
    let found = changed_at_latest(&mut app, (0, 0));
    assert!(
        found.contains(&format!(" at={} ", x.0)) && found.contains(" changed=yes bytes=7 "),
        "{found}"
    );
}

/// A witness on the shown state's own tick is judged by the view that shows it: a match reads faithful, a mismatch may differ.
#[test]
fn a_witness_on_the_shown_states_own_tick_is_judged() {
    use crate::rewind_verb::Asked;
    for poisoned in [false, true] {
        let mut app = scripted_app();
        start_recording(&app);
        let _ = slice(&app, b"before");
        let tie = {
            let s = app.pool.get(0).expect("session 0");
            let mut rec = s.ctx.temporal.lock().expect("recorder");
            let last = rec.latest_tick();
            while rec.now() <= last {}
            let tie = rec.now();
            rec.freeze_now_for_test(Some(tie));
            let live = s.live_term().clone();
            let mut term = term_lock(&live);
            let clock = ClockReading::now();
            term.process_at(b"\x1b[2J\x1b[Hshown", clock);
            rec.apply_inline(TemporalMsg::RawIn(InputSlice::at(
                b"\x1b[2J\x1b[Hshown",
                clock,
            )));
            let seen = if poisoned {
                let mut stranger = Terminal::new(term.rows(), term.cols());
                stranger.process(b"not what was on screen");
                stranger.checkpoint_visible()
            } else {
                term.checkpoint_visible()
            };
            rec.apply_inline(TemporalMsg::Witness(Arc::new(seen.expect("visible"))));
            rec.freeze_now_for_test(None);
            assert_eq!(
                (rec.latest_tick(), rec.fidelity_at(tie)),
                (tie, Fidelity::Unwitnessed),
                "fixture: the pending witness covering the state is on its tick"
            );
            tie
        };
        let open = ask(&mut app, Asked::Open(Target::At(tie))).expect("open");
        let want = if poisoned {
            " faithful=no "
        } else {
            " faithful=yes "
        };
        assert!(open.contains(want), "poisoned={poisoned}: {open}");
    }
}

/// The live keyframe that stands in for a slice is a state: open and End show the live screen.
#[test]
fn open_and_end_reach_the_live_screen_past_a_live_keyframe() {
    use crate::rewind_verb::Asked;
    let mut app = scripted_app();
    start_recording(&app);
    let _ = slice(&app, b"recorded");
    unrecorded(&app, b"\x1b[5;1Hunrecorded");
    witness(&app, &term_lock(&live_term(&app)));
    let _ = ask(&mut app, Asked::Open(Target::Latest)).expect("open");
    assert!(app.rewind_close(0));
    let newest = slice(&app, b"\r\nthe newest screen");
    let open = ask(&mut app, Asked::Open(Target::Latest)).expect("open");
    assert!(open.contains(&format!(" at={} ", newest.0)), "{open}");
    assert_eq!(shown_text(&app), live_text(&app), "the live screen");
    let _ = ask(&mut app, Asked::Step(StepBy::Slice(-1))).expect("back");
    assert_ne!(
        shown_text(&app),
        live_text(&app),
        "negative control: a state back is not"
    );
    let end = ask(&mut app, Asked::Step(StepBy::End(true))).expect("End");
    assert!(end.contains(&format!(" at={} ", newest.0)), "{end}");
    assert_eq!(shown_text(&app), live_text(&app));
}

/// An empty slice that ends no frame is no state of its own; one that ends a timed-out frame is.
#[test]
fn an_empty_slice_is_a_state_only_when_it_ends_a_frame() {
    let app = scripted_app();
    start_recording(&app);
    let a = slice(&app, b"a");
    let _ = slice(&app, b"");
    let b = slice(&app, b"b");
    let kept: Vec<Ticks> = instants(&app)[1..].iter().map(|i| i.at).collect();
    assert_eq!(kept, [a, b], "an empty slice repeats the screen before it");

    let app = scripted_app();
    start_recording(&app);
    let _ = slice(&app, b"\x1b[?2026hframe");
    let now = ClockReading::now();
    let late = ClockReading {
        monotonic: now.monotonic + std::time::Duration::from_secs(60),
        ..now
    };
    let ended = slice_clocked(&app, 0, b"", late);
    assert!(
        !term_lock(&live_term(&app)).modes().synchronized_output,
        "fixture: the empty parse timed the frame out"
    );
    let last = instants(&app).last().map(|i| i.at);
    assert_eq!(last, Some(ended), "the frame's end is a state");
}

/// A command start inside a frame the steps skip goes to the state the frame closes on, so Shift-Left still finds it.
#[test]
fn a_command_start_inside_a_skipped_frame_is_found_by_a_command_step() {
    use crate::rewind_verb::Asked;
    let mut app = scripted_app();
    start_recording(&app);
    let _ = slice(&app, FIRST_PROMPT);
    let _ = slice(&app, b"vim\r\n");
    let _ = slice(&app, b"\x1b]133;C\x07\x1b[?2026h\x1b[2J\x1b[Hhalf");
    let drawn = slice(&app, b" drawn\x1b[?2026l");
    let _ = slice(&app, b" more");
    let kept = instants(&app);
    assert!(
        kept.iter().any(|i| i.at == drawn && i.command),
        "the frame's close carries its command start: {kept:?}"
    );
    let _ = ask(&mut app, Asked::Open(Target::Latest)).expect("open");
    let back = ask(&mut app, Asked::Step(StepBy::Cmd(-1))).expect("Shift-Left");
    assert!(back.contains(&format!(" at={} ", drawn.0)), "{back}");
}

/// The past screen keeps no more scrollback than the session's own limit, and scrolled into it the band reads unchecked, since no witness saw it.
#[test]
fn the_past_scrollback_keeps_the_sessions_limit_and_reads_unchecked() {
    use crate::rewind_verb::Asked;
    let mut app = scripted_app();
    app.session_factory.terminal_config = Some(aterm_core::config::TerminalConfig {
        scrollback_limit: Some(200),
        ..aterm_core::config::TerminalConfig::default()
    });
    term_lock(&live_term(&app)).set_scrollback_line_limit(Some(200));
    start_recording(&app);
    let lines: String = (0..3000).map(|n| format!("line {n}\r\n")).collect();
    let past = slice(&app, lines.as_bytes());
    witness(&app, &term_lock(&live_term(&app)));
    let _ = slice(&app, b"\x1b[2J\x1b[Hthe live screen");
    let open = ask(&mut app, Asked::Open(Target::At(past))).expect("open");
    assert!(open.contains(" faithful=yes "), "fixture: {open}");
    app.rewind_scroll(0, 100_000);
    assert_eq!(view(&app).scrolled, 200, "the session's limit");
    let status = ask(&mut app, Asked::Status).expect("status");
    assert!(status.contains(" faithful=unchecked "), "{status}");
    app.rewind_scroll(0, -100_000);
    let status = ask(&mut app, Asked::Status).expect("status");
    assert!(
        status.contains(" faithful=yes "),
        "negative control: the witnessed screen: {status}"
    );
}

/// The fork cache books both grids of a fork: with the alternate screen up over a long styled history its byte cap binds before its count cap, and it books exactly the forks it holds.
#[test]
fn the_fork_cache_books_both_grids_and_its_byte_cap_binds() {
    use crate::rewind_worker::{MAX_FORK_BYTES, MAX_FORKS, STORE_LINES, fork_bytes, kept_after};
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
    let mut engine = plan.restore_bounded(STORE_LINES);
    let styled: String = (0..3100)
        .map(|n| {
            let cells: String = (0..80)
                .map(|c| format!("\x1b[38;2;{};{};{}m#", n % 251, c * 3 % 251, (n + c) % 251))
                .collect();
            cells + "\r\n"
        })
        .collect();
    engine.process(styled.as_bytes());
    engine.process(b"\x1b[?1049h\x1b[2J\x1b[Hvim");
    let each = fork_bytes(&engine);
    assert!(
        each.saturating_mul(MAX_FORKS) > MAX_FORK_BYTES,
        "fixture: {MAX_FORKS} forks of it pass the byte cap ({each} bytes each)"
    );
    let seqs: Vec<Seq> = (0..40).map(|n| Seq(n * 10)).collect();
    let (kept, booked, summed) = kept_after(&engine, &seqs, Seq(390));
    assert!(kept.len() < MAX_FORKS, "the byte cap binds: {}", kept.len());
    assert!(booked <= MAX_FORK_BYTES, "{booked}");
    assert_eq!(booked, summed, "every eviction refunds its fork");
}

/// The idle judge folds a part at a time and posts each part's verdicts, so a step waits for one part, not the whole chain.
#[test]
fn the_idle_judge_folds_and_posts_its_verdicts_in_parts() {
    let mut app = scripted_app();
    start_recording(&app);
    let mut last = Ticks(0);
    for k in 0..6 {
        let lines: String = (0..3000)
            .map(|n| format!("part {k} line {n:04} ......\r\n"))
            .collect();
        assert!(lines.len() > crate::rewind_worker::FORK_EVERY, "fixture");
        last = slice(&app, lines.as_bytes());
        witness(&app, &term_lock(&live_term(&app)));
    }
    let _ = slice(&app, b"\r\nafter");
    let rx = handed_view(&mut app);
    app.rewind_open(0, Target::At(last), None).expect("open");
    let shown = handed(&rx);
    assert!(matches!(shown, Done::Shown(_)), "{shown:?}");
    app.rewind_done(shown);
    let witnessed = |app: &App| {
        app.pool
            .get(0)
            .expect("session 0")
            .ctx
            .temporal
            .lock()
            .expect("recorder")
            .witnessed()
    };
    let mut parts = 0;
    while witnessed(&app) < 6 {
        let done = handed(&rx);
        assert!(matches!(done, Done::Judged { .. }), "{done:?}");
        app.rewind_done(done);
        parts += 1;
    }
    assert!(parts >= 2, "the judge posts in parts: {parts}");
    let status = ask(&mut app, crate::rewind_verb::Asked::Status).expect("status");
    assert!(status.contains(" faithful=yes "), "{status}");
}

/// The warm ends when the fork it wants lies more than a pass behind its front and past every fork it holds: each pass that stops short carries on from where it stopped.
#[test]
fn the_warm_ends_when_the_fork_it_wants_lies_far_behind_its_front() {
    use crate::rewind_worker::{FORK_EVERY, MAX_FORKS, warm_passes};
    let app = scripted_app();
    *app.pool
        .get(0)
        .expect("session 0")
        .ctx
        .temporal
        .lock()
        .expect("recorder") = crate::temporal::TemporalRecorder::with_budget(64 << 20);
    start_recording(&app);
    for k in 0..170 {
        let lines: String = (0..560)
            .map(|n| format!("slice {k:03} line {n:03} .......\r\n"))
            .collect();
        let _ = slice(&app, lines.as_bytes());
    }
    let plan = shown_at(&app, None).plan;
    let marks = plan.fork_marks(FORK_EVERY);
    assert!(
        marks.len() > MAX_FORKS + 2,
        "fixture: one chain with more marks than the warm wants ({} marks)",
        marks.len()
    );
    let (fork, end) = (marks[0], plan.end_seq());
    assert_ne!(
        warm_passes(plan, fork, end, end, 1_000),
        None,
        "the warm never ended"
    );
}

/// Steps from an idle view each fold at most a fork's spacing and a slice, across a keyframe too: the cache keeps forks along the chain before the shown one.
#[test]
fn each_step_folds_from_a_fork_within_its_spacing_across_two_chains() {
    use crate::rewind_worker::{FORK_EVERY, show_inline, warmed_shows};
    let app = scripted_app();
    start_recording(&app);
    let mut widest = 0;
    let mut chunk = |app: &App, k: usize| {
        let lines: String = (0..250)
            .map(|n| {
                format!(
                    "\x1b[3{}mchunk {k:02} line {n:03} {}\r\n",
                    n % 7,
                    "x".repeat(50)
                )
            })
            .collect();
        widest = widest.max(lines.len());
        let _ = slice(app, lines.as_bytes());
    };
    for k in 0..16 {
        chunk(&app, k);
    }
    start_recording(&app);
    for k in 16..32 {
        chunk(&app, k);
    }
    let kept = instants(&app);
    let shows: Vec<_> = kept
        .iter()
        .rev()
        .map(|i| shown_at(&app, Some(i.at)))
        .collect();
    let straight: Vec<String> = kept
        .iter()
        .rev()
        .map(|i| crate::control::screen_text(&show_inline(shown_at(&app, Some(i.at))).term))
        .collect();
    for (k, folded) in warmed_shows(shows).iter().enumerate() {
        assert!(
            folded.folded <= FORK_EVERY + widest,
            "step {k} folded {} bytes",
            folded.folded
        );
        assert_eq!(
            crate::control::screen_text(&folded.term),
            straight[k + 1],
            "step {k}"
        );
    }
}

/// `rewind check`'s line sums its chains, names the first failing chain's law, index and cursor, and shows that chain's digests, else the newest chain's.
#[test]
fn the_check_line_sums_its_chains_and_names_the_first_that_failed() {
    use aterm_aos::Digest;
    use aterm_aos::contract::Verdict;
    use aterm_aos_pane::trace::PaneCheck;
    use aterm_aos_pane::{PaneCode, PaneCounts};

    use crate::rewind_verb::check_line;

    let check = |n: u8, verdict: Verdict| PaneCheck {
        code: PaneCode::ThisBuild,
        start: Digest([n; 32]),
        state_digest: Digest([n + 1; 32]),
        observation_chain: Digest([n + 2; 32]),
        counts: PaneCounts {
            bytes: 2,
            resizes: 1,
            witnesses: 3,
            matched: 3,
        },
        replies_matched: 2,
        verdict,
    };
    let digests = |n: u8| {
        format!(
            "start={} state={} chain={}",
            Digest([n; 32]).short(),
            Digest([n + 1; 32]).short(),
            Digest([n + 2; 32]).short()
        )
    };
    let fail = |law: &'static str, cursor| Verdict::Fail {
        cursor,
        law,
        detail: String::new(),
    };
    assert_eq!(
        check_line(&[check(1, Verdict::Pass), check(4, Verdict::Pass)]),
        format!(
            "chains=2 slices=4 resizes=2 witnesses=6 matched=6 replies=4 verdict=pass {}",
            digests(4)
        ),
        "every chain passed: the newest chain's digests"
    );
    assert_eq!(
        check_line(&[
            check(1, Verdict::Pass),
            check(4, fail("RepliesMatch", 7)),
            check(8, fail("WitnessMatches", 2)),
        ]),
        format!(
            "chains=3 slices=6 resizes=3 witnesses=9 matched=9 replies=6 \
             verdict=fail:RepliesMatch@1:7 {}",
            digests(4)
        ),
        "the first failing chain's law, index, cursor and digests"
    );
}

/// Rewind restores inline (OSC 1337), scrolled-history, and Kitty cell/relative/floating placements on `shown_term` even after the live screen cleared and deleted them across a keyframe.
#[test]
fn rewind_view_renders_past_inline_and_kitty_images_across_keyframes_and_scrollback() {
    use crate::rewind_verb::Asked;
    use aterm_aos::contract::Verdict;
    use aterm_aos_pane::PaneCode;

    let mut app = scripted_app();
    term_lock(&live_term(&app)).set_cell_pixel_size(10, 20);
    start_recording(&app);

    let mut first =
        b"\x1b]1337;File=inline=1;width=3;height=2;preserveAspectRatio=0:QUJDREVGRw==\x1b\\\
                      \x1b[24;1H\r\n\
                      \x1b_Ga=t,f=32,s=2,v=1,i=1;////AAAA//8=\x1b\\\
                      \x1b[3;2H\x1b_Ga=p,i=1,p=10,c=2,r=1\x1b\\\
                      \x1b_Ga=p,i=1,p=11,P=1,Q=10,H=3,V=0,c=1,r=1\x1b\\\
                      \x1b_Ga=p,U=1,i=1,p=2,c=2,r=1\x1b\\"
            .to_vec();
    first.extend_from_slice(
        "\x1b[4;2H\x1b[38;5;1;58;5;2m\u{10EEEE}\u{0305}\u{0305}\x1b[39;59m".as_bytes(),
    );
    first.extend_from_slice(b"\x1b_Ga=p,i=1,p=21,P=1,Q=2,H=1,V=0,c=1,r=1\x1b\\");
    let past_img = slice(&app, &first);
    witness(&app, &term_lock(&live_term(&app)));

    // Mint a mid-session keyframe carrying the placed OSC 1337 + Kitty store/relations/floating.
    {
        let s = app.pool.get(0).expect("session 0");
        let kf = {
            let term = term_lock(s.live_term());
            crate::temporal::keyframe_of(&term, usize::MAX).expect("keyframe")
        };
        assert!(kf.carry.retained_image_bytes() > 0);
        s.ctx.temporal.lock().expect("recorder").record_keyframe(kf);
    }

    // Move parent p=10 after the keyframe (cascades to p=11), then wipe the live screen.
    let moved = slice(&app, b"\x1b[3;7H\x1b_Ga=p,i=1,p=10,c=2,r=1\x1b\\");
    witness(&app, &term_lock(&live_term(&app)));
    let _ = slice(&app, b"\x1b[2J\x1b[H\x1b_Ga=d,d=A\x1b\\live text only");
    witness(&app, &term_lock(&live_term(&app)));

    assert!(
        term_lock(&live_term(&app))
            .grid()
            .capture_image_carry(0)
            .visible
            .is_empty(),
        "fixture: live screen cleared all visible images"
    );

    let open = ask(&mut app, Asked::Open(Target::At(moved))).expect("open at moved");
    assert!(open.contains(" faithful=yes "), "{open}");
    {
        let shown_arc = app.pool.shown_term(0).expect("shown term");
        let shown = term_lock(&shown_arc);
        let img_carry = shown.grid().capture_image_carry(usize::MAX);
        assert!(
            !img_carry.visible.is_empty() && !img_carry.history_rev.is_empty(),
            "shown_term carries both visible and scrolled-history images: {img_carry:?}"
        );
        // Moved parent p=10 at row 2 col 6 (1-based 3;7), cascaded child p=11 at row 2 col 9 (6 + H=3).
        let row2 = &img_carry
            .visible
            .iter()
            .find(|(r, _)| *r == 2)
            .expect("row 2 has moved Kitty placements")
            .1;
        let cols: Vec<(u16, u16, u32)> = row2
            .iter()
            .map(|sp| (sp.start_col, sp.end_col, sp.kitty.unwrap().placement_id))
            .collect();
        // Moving p=10 mints a newer placement serial for p=10 while restamping p=11 with its
        // original serial, so layer_order sorts p=11 before p=10.
        assert_eq!(cols, vec![(9, 10, 11), (6, 8, 10)]);
    }

    let back = ask(&mut app, Asked::Open(Target::At(past_img))).expect("open at past_img");
    assert!(back.contains(" faithful=yes "), "{back}");
    {
        let shown_arc = app.pool.shown_term(0).expect("shown term");
        let shown = term_lock(&shown_arc);
        let img_carry = shown.grid().capture_image_carry(usize::MAX);
        let row2 = &img_carry
            .visible
            .iter()
            .find(|(r, _)| *r == 2)
            .expect("row 2 has pre-move Kitty placements")
            .1;
        let cols: Vec<(u16, u16, u32)> = row2
            .iter()
            .map(|sp| (sp.start_col, sp.end_col, sp.kitty.unwrap().placement_id))
            .collect();
        assert_eq!(cols, vec![(1, 3, 10), (4, 5, 11)]);
    }

    let chains = app
        .pool
        .get(0)
        .expect("session 0")
        .ctx
        .temporal
        .lock()
        .expect("recorder")
        .pane_chains()
        .expect("pane chains");
    assert_eq!(chains.len(), 2);
    for (idx, chain) in chains.iter().enumerate() {
        let check = chain.check(PaneCode::ThisBuild, "test");
        assert_eq!(check.verdict, Verdict::Pass, "chain {idx}: {check:?}");
    }
}

/// Autonomous Kitty animation frame steps record `HostOp::AdvanceKittyAnimations` as steppable
/// Rewind instants, match live witnesses (`diverged == 0`), and pass `pane_chains` (`rewind check`).
#[test]
fn autonomous_kitty_animation_ticks_step_rewind_instants_and_pass_pane_chains() {
    use crate::rewind_verb::Asked;
    use aterm_aos::contract::Verdict;
    use aterm_aos_pane::PaneCode;

    let mut app = scripted_app();
    let t0 = std::time::Instant::now();
    let ms = |n: u64| t0 + std::time::Duration::from_millis(n);
    term_lock(&live_term(&app)).set_cell_pixel_size(10, 20);
    start_recording(&app);

    let spine = app
        .pool
        .get(0)
        .expect("session 0")
        .ctx
        .temporal
        .lock()
        .expect("recorder")
        .spine_watermark();
    let (tx, rx) = crate::temporal::queue_on(spine, 64);
    app.pool.set_temporal_tx_for_test(0, Some(tx));

    let apc = |control: &str, payload: &[u8]| {
        let mut seq = format!("\x1b_G{control}").into_bytes();
        if !payload.is_empty() {
            seq.push(b';');
            seq.extend_from_slice(
                aterm_codec::base64::encode(payload)
                    .expect("encode")
                    .as_bytes(),
            );
        }
        seq.extend_from_slice(b"\x1b\\");
        seq
    };
    let mut setup = Vec::new();
    setup.extend_from_slice(&apc("a=t,f=32,s=10,v=20,i=7,q=2", &[10; 800]));
    setup.extend_from_slice(&apc("a=f,f=32,s=10,v=20,i=7,z=30,q=2", &[20; 800]));
    setup.extend_from_slice(&apc("a=a,i=7,r=1,z=30", b""));
    setup.extend_from_slice(&apc("a=p,i=7,C=1,q=2", b""));
    setup.extend_from_slice(&apc("a=a,i=7,s=3", b""));
    let frame1_tick = slice_clocked(
        &app,
        0,
        &setup,
        ClockReading {
            monotonic: t0,
            wall_ms: None,
        },
    );
    witness(&app, &term_lock(&live_term(&app)));

    let drain_tick = |app: &mut App, at: std::time::Instant| -> Ticks {
        advance_recording_clocks(app, &[0], 5);
        app.service_kitty_animations(at);
        let s = app.pool.get(0).expect("session 0");
        let mut rec = s.ctx.temporal.lock().expect("recorder");
        while let Some(msg) = rx.try_recv() {
            rec.apply_inline(msg);
        }
        rec.latest_tick()
    };

    app.tick_kitty_animations(0, ms(10));
    let frame2_tick = drain_tick(&mut app, ms(30));
    witness(&app, &term_lock(&live_term(&app)));
    let wrap_tick = drain_tick(&mut app, ms(60));
    witness(&app, &term_lock(&live_term(&app)));

    let shown_pixel = |app: &App| {
        let shown_arc = app.pool.shown_term(0).expect("shown term");
        let shown = term_lock(&shown_arc);
        shown.images_row(0)[0].1.image.bytes[0]
    };

    for (tick, pixel) in [(frame1_tick, 10u8), (frame2_tick, 20u8), (wrap_tick, 10u8)] {
        let reply = ask(&mut app, Asked::Open(Target::At(tick))).expect("open at tick");
        assert!(reply.contains(" faithful=yes "), "{tick:?}: {reply}");
        assert_eq!(
            shown_pixel(&app),
            pixel,
            "wrong animation frame at {tick:?}"
        );
    }

    let (diverged, chains) = {
        let s = app.pool.get(0).expect("session 0");
        let rec = s.ctx.temporal.lock().expect("recorder");
        (rec.diverged(), rec.pane_chains().expect("chains"))
    };
    assert_eq!(diverged, 0);
    for (idx, chain) in chains.iter().enumerate() {
        let check = chain.check(PaneCode::ThisBuild, "test");
        assert_eq!(check.verdict, Verdict::Pass, "chain {idx}: {check:?}");
    }
}

fn zlib_zeros(len: usize) -> Vec<u8> {
    let mut bits: Vec<u8> = Vec::new();
    let mut field = |value: u32, n: u32| bits.extend((0..n).map(|i| ((value >> i) & 1) as u8));
    let code = |code: u32, n: u32| (0..n).fold(0, |r, i| r | ((code >> i) & 1) << (n - 1 - i));
    field(0b011, 3);
    field(code(0x30, 8), 8);
    let mut left = len - 1;
    while left >= 258 {
        field(code(0b1100_0101, 8), 8);
        field(0, 5);
        left -= 258;
    }
    for _ in 0..left {
        field(code(0x30, 8), 8);
    }
    field(0, 7);
    let mut out = vec![0x78, 0x01];
    out.extend(bits.chunks(8).map(|byte| {
        byte.iter()
            .enumerate()
            .fold(0u8, |acc, (i, &bit)| acc | (bit << i))
    }));
    let b = u32::try_from(len % 65521).expect("below 65521");
    out.extend_from_slice(&((b << 16) | 1).to_be_bytes());
    out
}

/// The fork cache books unbooked Kitty store and visible images alongside grid memory so image-heavy
/// chains respect `MAX_FORK_BYTES` now that `replay_fork` carries the Kitty store.
#[test]
fn the_fork_cache_books_unbooked_kitty_and_visible_images_under_its_byte_cap() {
    use crate::rewind_worker::{MAX_FORK_BYTES, MAX_FORKS, STORE_LINES, fork_bytes, kept_after};
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
    let mut engine = plan.restore_bounded(STORE_LINES);
    let side = 720usize;
    let raw_bytes = side * side * 4; // ~2.07 MiB uncompressed RGBA per fork
    let compressed = aterm_codec::base64::encode(&zlib_zeros(raw_bytes)).expect("encode");
    let apc = format!("\x1b_Ga=t,f=32,s={side},v={side},o=z,i=9,q=2;{compressed}\x1b\\");
    engine.process(apc.as_bytes());
    assert!(engine.fork().is_some(), "fork succeeds with Kitty store");

    let each = fork_bytes(&engine);
    assert!(
        each >= raw_bytes && each.saturating_mul(MAX_FORKS) > MAX_FORK_BYTES,
        "fixture: each fork books its ~2 MiB Kitty store ({each} bytes)"
    );
    let seqs: Vec<Seq> = (0..40).map(|n| Seq(n * 10)).collect();
    let (kept, booked, summed) = kept_after(&engine, &seqs, Seq(390));
    assert!(kept.len() < MAX_FORKS, "the byte cap binds: {}", kept.len());
    assert!(booked <= MAX_FORK_BYTES, "{booked}");
    assert_eq!(booked, summed, "every eviction refunds its fork");
}
