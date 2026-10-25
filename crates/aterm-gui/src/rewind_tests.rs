// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rewind: where a view opens and steps, what its band says, what it withholds, and what it draws.

use super::*;
use crate::input::InputEvent;
use crate::scripted_redraw_fixture::{scripted_app, surface};
use crate::term_lock;
use aterm_core::render::RenderInput;
use aterm_core::terminal::RenderCell;
use aterm_types::keyboard::{Key, KeyEventType, Modifiers};
use aterm_window::keyboard::{Key as WinitKey, ModifiersState, NamedKey};

const WID: WindowId = WindowId(0);

fn t(us: u64) -> Ticks {
    Ticks(us)
}

#[test]
fn a_view_opens_at_or_before_its_target_and_refuses_before_the_oldest_state() {
    let instants = [t(100), t(200), t(300)];
    let now = t(5_000_000);
    assert_eq!(place(&instants, Target::Latest, now), Ok(2));
    assert_eq!(place(&instants, Target::At(t(250)), now), Ok(1));
    assert_eq!(place(&instants, Target::At(t(300)), now), Ok(2));
    assert_eq!(place(&instants, Target::At(t(9_999)), now), Ok(2));
    assert_eq!(
        place(&[t(4_000_000), t(4_600_000)], Target::Ago(1), now),
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
fn a_step_moves_among_the_recorded_states_and_stops_at_either_end() {
    let instants = [t(100), t(200), t(300)];
    assert_eq!(step_index(&instants, t(300), -1), Some(1));
    assert_eq!(step_index(&instants, t(250), 1), Some(2));
    assert_eq!(step_index(&instants, t(100), -5), Some(0));
    assert_eq!(step_index(&instants, t(100), 7), Some(2));
    assert_eq!(step_index(&[], t(100), 1), None);
}

#[test]
fn left_right_step_esc_closes_a_command_chord_passes_and_the_rest_is_withheld() {
    let plain = ModifiersState::empty();
    let left = WinitKey::Named(NamedKey::ArrowLeft);
    let right = WinitKey::Named(NamedKey::ArrowRight);
    let esc = WinitKey::Named(NamedKey::Escape);
    assert_eq!(key_use(&left, plain, false), KeyUse::Step(-1));
    assert_eq!(key_use(&right, plain, false), KeyUse::Step(1));
    assert_eq!(key_use(&esc, plain, false), KeyUse::Close);
    let a = WinitKey::Character("a".into());
    assert_eq!(key_use(&a, plain, false), KeyUse::Withhold);
    assert_eq!(
        key_use(&left, ModifiersState::SHIFT, false),
        KeyUse::Withhold
    );
    assert_eq!(key_use(&a, ModifiersState::SUPER, false), KeyUse::Pass);
    assert_eq!(key_use(&a, plain, true), KeyUse::Pass, "a modifier press");
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
        band_text(&view, 1_042_000, 3_600, 1_000),
        "REWIND  01:16:40 (-0:42)  state 803/1412  unchecked  Left/Right step  Esc live"
    );
    assert_eq!(
        band_text(&view, 1_042_000, 3_600, 1_000 + 3_000),
        "REWIND  01:16:40 (-0:42)  state 803/1412  unchecked  live +3 KB  Left/Right step  Esc \
         live"
    );
    assert_eq!(
        status_line(true, Some(&view), 1_500),
        "recording=true view=open at=1200 state=803/1412 faithful=unchecked live_bytes=500"
    );
    let differs = view_at((1, 2), Faith::MayDiffer);
    assert!(
        band_text(&differs, 1_000_000, 0, 0).contains("  may differ from what was on screen  "),
        "{}",
        band_text(&differs, 1_000_000, 0, 0)
    );
    assert!(status_line(true, Some(&differs), 0).contains(" faithful=no "));
    assert!(!band_text(&differs, 1_000_000, 0, 0).contains("faithful"));
    assert_eq!(
        status_line(false, None, 7),
        "recording=false view=closed at=- state=- faithful=- live_bytes=0"
    );
    assert_eq!(ago(3_725), "-1:02:05");
    assert_eq!(clock(-1_000, 0), "23:59:59");
}

/// Start recording session 0 from its screen now: a keyframe of the live engine seeded on the spine.
fn start_recording(app: &App) {
    let s = app.pool.get(0).expect("session 0");
    let (checkpoint, pre) = {
        let term = term_lock(s.live_term());
        (term.checkpoint(), crate::temporal::SpinePre::take(&term))
    };
    let mut rec = s.ctx.temporal.lock().expect("recorder");
    rec.record_keyframe(checkpoint);
    rec.spine_watermark().seed(pre);
}

/// Feed `bytes` to session 0's engine and recorder as one slice at a later instant than any before it; that instant.
fn slice(app: &App, bytes: &[u8]) -> Ticks {
    let s = app.pool.get(0).expect("session 0");
    let mut rec = s.ctx.temporal.lock().expect("recorder");
    let last = rec.latest_tick();
    while rec.now() <= last {}
    term_lock(s.live_term()).process(bytes);
    rec.record_raw_in(bytes);
    rec.latest_tick()
}

/// Record a live witness of `term`'s screen on session 0's spine, at a later instant than any before it, as the reader does.
fn witness(app: &App, term: &Terminal) {
    let s = app.pool.get(0).expect("session 0");
    let mut rec = s.ctx.temporal.lock().expect("recorder");
    let last = rec.latest_tick();
    while rec.now() <= last {}
    let live = term.checkpoint_visible().expect("a visible checkpoint");
    rec.apply_inline(crate::temporal::TemporalMsg::Witness(Arc::new(live)));
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
    crate::rewind_worker::fold(0, 0, (plan, None), None).term
}

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

fn esc() -> WinitKey {
    WinitKey::Named(NamedKey::Escape)
}

/// R0 exit item 4: a rewound pane draws exactly the replayed engine's cells, and Esc draws the live engine's again.
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
        &shown[..rows - 1],
        &want_past[..rows - 1],
        "the replayed cells"
    );
    assert_ne!(
        text(&shown[..rows - 1]),
        text(&want_live[..rows - 1]),
        "negative control: the rewound frame is not the live screen"
    );
    let band = text(&shown[rows - 1..]);
    assert!(
        band.starts_with(" REWIND  ") && band.contains("  state 2/3  unchecked  "),
        "the band: {band:?}"
    );
    assert!(
        !band.contains("faithful"),
        "no witness, no faithful: {band:?}"
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
    let live = app.pool.get(0).expect("session 0").live_term().clone();
    assert!(
        crate::control::screen_text(&term_lock(&live)).starts_with("the live screen now"),
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
    let stepped = ask(&mut app, Asked::Step(1)).expect("step");
    assert!(stepped.contains(" state=3/4 "), "{stepped}");
    let shown = ask(&mut app, Asked::Text { trim: true }).expect("text");
    assert!(shown.ends_with("\none\ntwo\n"), "{shown:?}");
    let live = app.pool.get(0).expect("session 0").live_term().clone();
    assert!(
        crate::control::screen_text(&term_lock(&live)).contains("three"),
        "negative control: the live screen moved on"
    );
    assert!(
        ask(&mut app, Asked::Step(-9))
            .expect("step")
            .contains(" state=1/4 ")
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
    assert_eq!(ask(&mut app, Asked::Step(1)), Err(NO_VIEW.to_string()));
}

/// The view says faithful only where a live witness covering the shown state matched its replay, and may differ from a mismatch on.
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

    let stepped = ask(&mut app, Asked::Step(1)).expect("step");
    assert!(stepped.contains(&format!(" at={} ", two.0)), "{stepped}");
    assert!(stepped.contains(" faithful=no "), "{stepped}");
    app.redraw_window(WID);
    let band = text(&drawn(&app)[rows - 1..]);
    assert!(
        band.contains("  may differ from what was on screen  ") && !band.contains("faithful"),
        "{band:?}"
    );
    let stepped = ask(&mut app, Asked::Step(1)).expect("step");
    assert!(stepped.contains(&format!(" at={} ", three.0)), "{stepped}");
    assert!(
        stepped.contains(" faithful=no "),
        "a state after a mismatch may differ too: {stepped}"
    );
    let back = ask(&mut app, Asked::Step(-2)).expect("step");
    assert!(back.contains(" faithful=yes "), "{back}");
}

#[test]
fn the_rewind_instants_are_the_oldest_keyframe_then_each_later_slice() {
    let app = scripted_app();
    let rec = || app.pool.get(0).expect("session 0").ctx.temporal.clone();
    assert!(rec().lock().expect("recorder").rewind_instants().is_empty());
    start_recording(&app);
    let a = slice(&app, b"a");
    let b = slice(&app, b"b");
    let instants = rec().lock().expect("recorder").rewind_instants();
    assert_eq!(instants.len(), 3, "{instants:?}");
    assert_eq!(&instants[1..], &[a, b]);
    assert!(instants[0] < a);
}
