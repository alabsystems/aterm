// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stats lens and the sill driven through the App's own routing
//! (`on_cursor_moved`, `on_mouse_input`, `on_mouse_wheel`, `on_key`,
//! `on_focus` and the config apply), each read off what the tab's program
//! received: the lens opening on a rest and leaving with the pointer, a
//! press pinning it, a press outside closing it and going on, focus loss
//! closing it, the keyboard lens taking its own keys while any other key
//! closes it and reaches the terminal, the sill's instruments taking the
//! pointer before the grid does, and View ▸ Stats Bars' checkmark and write.

use std::io::{ErrorKind, Read};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aterm_session::sink::SinkWriter;
use aterm_window::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta};
use aterm_window::keyboard::{Key, KeyCode, KeyLocation, NamedKey, PhysicalKey, SmolStr};

use super::{LEAVE, Lens, Opened};
use crate::input::{InputEvent, PasteFraming, Source};
use crate::menu::{MenuAction, native_menu_checked};
use crate::stats_deck::StatsBars;
use crate::stats_paint::{Bar, Hit, Instrument};
use crate::{App, WindowId, term_lock};

/// A private observer of the session's PTY input: the read end, and a sink
/// writing into the other end.
fn observer() -> (UnixStream, Arc<SinkWriter>) {
    let (reader, writer) = UnixStream::pair().expect("private input observer");
    reader.set_nonblocking(true).unwrap();
    (reader, Arc::new(SinkWriter::new_owned(writer.into())))
}

fn drain(reader: &mut UnixStream) -> String {
    let mut out = Vec::new();
    let mut bytes = [0u8; 256];
    loop {
        match reader.read(&mut bytes) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&bytes[..n]),
            Err(e) if e.kind() == ErrorKind::WouldBlock => break,
            Err(e) => panic!("reading input: {e}"),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// One window drawing `form`, its session writing to a private wire, with
/// a snapshot to draw and a 24 px bottom pad for the sill.
struct Rig {
    app: App,
    wid: WindowId,
    session: u64,
    wire: UnixStream,
    _attached: crate::stats_deck::tests::Attached,
}

fn rig(form: &str) -> Rig {
    let attached = crate::stats_deck::tests::attach_stats();
    let (mut wire, sink) = observer();
    let mut app = App::headless_for_test_with_sink(sink);
    let wid = WindowId(0);
    app.config
        .sys
        .get_or_insert_with(Default::default)
        .stats_bars = Some(form.into());
    app.windows.get_mut(&wid).expect("window").metrics.pad = 24;
    app.vitals.snapshot = Some(Arc::new(crate::stats_deck::tests::snapshot()));
    let session = app.focused_session_id(wid).expect("the tab's terminal");
    let _ = drain(&mut wire);
    Rig {
        app,
        wid,
        session,
        wire,
        _attached: attached,
    }
}

impl Rig {
    /// Everything the program was sent before a marker pasted to it now: the
    /// marker rides the session's one ordered writer, so anything a gesture
    /// sent is in the text and nothing can arrive later.
    fn sent(&mut self) -> String {
        const MARK: &str = "<marker>";
        self.app.input_to_session(
            self.wid,
            InputEvent::Paste(MARK.to_owned(), PasteFraming::Gesture { bracketed: false }),
            Source::Controller,
            Some(self.session),
            crate::app_input::PressPhase::Initial,
        );
        // A hang detector, not a latency budget.
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut seen = String::new();
        loop {
            seen += &drain(&mut self.wire);
            if let Some(before) = seen.strip_suffix(MARK) {
                return before.to_owned();
            }
            assert!(
                Instant::now() < deadline,
                "the marker never arrived: {seen:?}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Turn on the program's mouse reporting (`modes`, e.g. SGR any-motion).
    fn report_mouse(&mut self, modes: &[u8]) {
        let term = self
            .app
            .pool
            .get(self.session)
            .expect("live session")
            .term
            .clone();
        term_lock(&term).process(modes);
        let _ = self.sent();
    }

    fn lens(&self) -> Option<Lens> {
        self.app.windows[&self.wid].stats_lens.open
    }

    /// The frame point at the centre of the sill's first `want` instrument.
    fn instrument(&self, want: Instrument) -> ((f64, f64), Hit) {
        let _ = self.app.stats_deck_fp(self.wid);
        let f = self.app.deck_frame(self.wid).expect("the form shows");
        assert_eq!(f.form, StatsBars::Sill);
        let hit = self.app.windows[&self.wid]
            .stats_hits
            .borrow()
            .iter()
            .find(|h| h.instrument == want)
            .copied()
            .expect("the instrument is on the sill");
        let (x, y, w, h) = hit.rect;
        let (ox, oy) = self.app.frame_origin(self.wid);
        let at = (
            ox as f64 + f64::from(x + w / 2.0),
            oy as f64 + f64::from(f.dy) + f64::from(y + h / 2.0),
        );
        (at, hit)
    }

    /// The frame point at the centre of grid cell `(row, col)`.
    fn cell(&self, row: u16, col: u16) -> (f64, f64) {
        self.app
            .cell_centre_px(self.wid, row, col)
            .expect("cell on the grid")
    }

    /// The frame point at the centre of the open lens's card.
    fn over_lens(&mut self) -> (f64, f64) {
        self.app.splice_stats_lens(self.wid);
        let (dx, dy, w, h) = self.app.windows[&self.wid]
            .stats_lens
            .rect
            .expect("the lens is on screen");
        let (ox, oy) = self.app.frame_origin(self.wid);
        (
            ox as f64 + f64::from(dx + w / 2),
            oy as f64 + f64::from(dy + h / 2),
        )
    }

    fn move_to(&mut self, (x, y): (f64, f64)) {
        self.app.on_cursor_moved(self.wid, x, y);
    }

    fn click(&mut self, button: MouseButton) {
        self.app
            .on_mouse_input(self.wid, ElementState::Pressed, button);
        self.app
            .on_mouse_input(self.wid, ElementState::Released, button);
    }

    /// Rest on `want` until the rest is served: the hover lens opens there.
    fn rest_on(&mut self, want: Instrument) -> Hit {
        let (at, hit) = self.instrument(want);
        self.move_to(at);
        let due = self.app.stats_lens_deadline().expect("the rest's deadline");
        self.app.service_stats_lens(due);
        hit
    }

    fn key(&mut self, code: KeyCode, logical: Key, text: Option<&str>) {
        for state in [ElementState::Pressed, ElementState::Released] {
            let ev = KeyEvent::synthetic_for_test(
                PhysicalKey::Code(code),
                logical.clone(),
                text.map(SmolStr::new),
                KeyLocation::Standard,
                state,
                false,
            );
            self.app.on_key(self.wid, ev);
        }
    }

    fn named(&mut self, code: KeyCode, key: NamedKey) {
        self.key(code, Key::Named(key), key.to_text());
    }
}

/// A pointer resting on an instrument opens the hover lens there when the
/// rest is served, not before; a pointer passing over opens none.
#[test]
fn a_rest_opens_the_hover_lens_and_passing_over_opens_none() {
    let mut r = rig("sill");
    let (on, hit) = r.instrument(Instrument::Cores);
    let away = r.cell(0, 0);

    r.move_to(on);
    assert!(
        r.app.windows[&r.wid].stats_lens.rest.is_some(),
        "a rest is armed"
    );
    r.move_to(away);
    assert_eq!(r.app.stats_lens_deadline(), None, "moving on disarms it");
    r.app.service_stats_lens(Instant::now() + LEAVE * 4);
    assert_eq!(r.lens(), None, "passing over opens no lens");

    r.move_to(on);
    let due = r.app.stats_lens_deadline().expect("the rest's deadline");
    r.app.service_stats_lens(due - Duration::from_millis(1));
    assert_eq!(r.lens(), None, "not before the rest completes");
    r.app.service_stats_lens(due);
    assert_eq!(
        r.lens(),
        Some(Lens {
            focus: hit.instrument,
            bar: hit.bar,
            how: Opened::Hover,
        })
    );
}

/// A hover lens leaves with the pointer once its leave runs out, and stays
/// while the pointer is over the lens itself.
#[test]
fn a_hover_lens_leaves_with_the_pointer_and_stays_while_over_the_lens() {
    let mut r = rig("sill");
    r.rest_on(Instrument::Cores);
    assert!(r.lens().is_some_and(|l| l.how == Opened::Hover));
    let lens = r.over_lens();
    let away = r.cell(0, 0);
    let (dx, dy, w, h) = r.app.windows[&r.wid].stats_lens.rect.expect("on screen");
    assert!(
        !(f64::from(dx)..f64::from(dx + w)).contains(&away.0)
            || !(f64::from(dy)..f64::from(dy + h)).contains(&away.1),
        "PRECONDITION: the grid's first cell is clear of the lens"
    );

    r.move_to(away);
    assert!(
        r.app.windows[&r.wid].stats_lens.leave_due.is_some(),
        "the leave starts"
    );
    r.move_to(lens);
    assert_eq!(
        r.app.windows[&r.wid].stats_lens.leave_due, None,
        "back over the lens, the leave is off"
    );
    r.app.service_stats_lens(Instant::now() + LEAVE * 4);
    assert!(r.lens().is_some(), "the lens stays under the pointer");

    r.move_to(away);
    let due = r.app.stats_lens_deadline().expect("the leave's deadline");
    r.app.service_stats_lens(due);
    assert_eq!(r.lens(), None, "the lens leaves with the pointer");
}

/// A press on an instrument pins the lens there and reaches no program; a
/// pinned lens outlasts the pointer; a press outside closes it and goes on
/// to the program under the pointer.
#[test]
fn a_press_pins_the_lens_and_a_press_outside_closes_it_and_goes_on() {
    let mut r = rig("sill");
    r.report_mouse(b"\x1b[?1000h\x1b[?1006h");
    let (on, hit) = r.instrument(Instrument::Cores);

    r.move_to(on);
    r.click(MouseButton::Left);
    assert_eq!(
        r.lens(),
        Some(Lens {
            focus: hit.instrument,
            bar: hit.bar,
            how: Opened::Pinned,
        })
    );
    assert_eq!(
        r.sent(),
        "",
        "the press on the instrument reaches no program"
    );

    r.move_to(r.cell(0, 0));
    r.app.service_stats_lens(Instant::now() + LEAVE * 4);
    assert!(r.lens().is_some(), "a pinned lens outlasts the pointer");

    r.click(MouseButton::Left);
    assert_eq!(r.lens(), None, "a press outside closes it");
    assert_eq!(
        r.sent(),
        "\x1b[<0;1;1M\x1b[<0;1;1m",
        "and goes on to the program"
    );
}

/// Any lens closes when its window loses focus.
#[test]
fn focus_loss_closes_the_lens() {
    let mut r = rig("sill");
    assert!(r.app.toggle_stats_lens());
    assert!(r.lens().is_some());
    r.app.on_focus(r.wid, false);
    assert_eq!(r.lens(), None);
}

/// The keyboard lens (⌘I) takes ←/→/↑/↓, Return and Esc, and a bare
/// modifier is inert; any other key closes it and reaches the terminal, as
/// does an arrow once Return has pinned it.
#[test]
fn the_keyboard_lens_takes_its_keys_and_any_other_key_closes_it_and_goes_on() {
    let mut r = rig("sill");
    assert!(r.app.toggle_stats_lens());
    let at = |focus, bar| {
        Some(Lens {
            focus,
            bar,
            how: Opened::Keyboard,
        })
    };
    assert_eq!(r.lens(), at(Instrument::Cores, Bar::Global));
    r.named(KeyCode::ArrowRight, NamedKey::ArrowRight);
    assert_eq!(r.lens(), at(Instrument::Gpu, Bar::Global));
    r.named(KeyCode::ArrowLeft, NamedKey::ArrowLeft);
    r.named(KeyCode::ArrowLeft, NamedKey::ArrowLeft);
    assert_eq!(
        r.lens(),
        at(Instrument::Jobs, Bar::Global),
        "the focus wraps"
    );
    r.named(KeyCode::ArrowDown, NamedKey::ArrowDown);
    assert_eq!(r.lens(), at(Instrument::Jobs, Bar::Aterm));
    r.named(KeyCode::ArrowUp, NamedKey::ArrowUp);
    r.named(KeyCode::ArrowUp, NamedKey::ArrowUp);
    assert_eq!(r.lens(), at(Instrument::Jobs, Bar::Tab), "the bar wraps");
    r.key(
        KeyCode::ShiftLeft,
        Key::Named(NamedKey::Shift),
        NamedKey::Shift.to_text(),
    );
    assert_eq!(
        r.lens(),
        at(Instrument::Jobs, Bar::Tab),
        "a bare modifier is inert"
    );
    assert_eq!(r.sent(), "", "the lens's keys reach no program");

    r.key(KeyCode::KeyX, Key::Character(SmolStr::new("x")), Some("x"));
    assert_eq!(r.lens(), None, "another key closes the lens");
    assert_eq!(r.sent(), "x", "and reaches the terminal");

    assert!(r.app.toggle_stats_lens());
    r.named(KeyCode::Escape, NamedKey::Escape);
    assert_eq!(r.lens(), None, "Esc closes it");
    assert_eq!(r.sent(), "", "and is the lens's");

    assert!(r.app.toggle_stats_lens());
    r.named(KeyCode::Enter, NamedKey::Enter);
    assert!(
        r.lens().is_some_and(|l| l.how == Opened::Pinned),
        "Return pins it"
    );
    r.named(KeyCode::ArrowRight, NamedKey::ArrowRight);
    assert_eq!(r.lens(), None, "a pinned lens closes on a key");
    assert_eq!(r.sent(), "\x1b[C", "which goes on to the terminal");
}

/// No lens opens in a window too small to show one, and an open lens closes
/// when the window shrinks under it, so no invisible lens swallows the
/// arrows.
#[test]
fn no_lens_opens_or_stays_where_none_can_show() {
    let mut r = rig("sill");
    assert!(r.app.toggle_stats_lens());
    r.app.windows.get_mut(&r.wid).expect("window").rows = 4;
    assert!(
        !r.app.stats_lens_fits(r.wid),
        "PRECONDITION: four rows hold no lens"
    );
    r.app.splice_stats_lens(r.wid);
    assert_eq!(r.lens(), None, "the shrunk window's lens closes");
    r.named(KeyCode::ArrowRight, NamedKey::ArrowRight);
    assert_eq!(r.sent(), "\x1b[C", "the arrow reaches the terminal");
    assert!(!r.app.toggle_stats_lens(), "⌘I opens none");
    assert_eq!(r.lens(), None);
}

/// The sill's instruments take the pointer before the grid does: a program
/// reporting every motion sees none over an instrument, nor its wheel or a
/// right press, and sees all of them over the grid next to it.
#[test]
fn the_sills_instruments_take_motion_presses_and_the_wheel_before_the_grid() {
    let mut r = rig("sill");
    r.report_mouse(b"\x1b[?1003h\x1b[?1006h");
    let (on, _) = r.instrument(Instrument::Cores);
    let last_row = r.app.windows[&r.wid].rows - 1;

    // CONTROL: over the grid's last row, the program sees each gesture.
    r.move_to(r.cell(last_row, 5));
    r.app
        .on_mouse_wheel(r.wid, MouseScrollDelta::LineDelta(0.0, 1.0));
    r.click(MouseButton::Right);
    let row = last_row + 1;
    assert_eq!(
        r.sent(),
        format!("\x1b[<35;6;{row}M\x1b[<64;6;{row}M\x1b[<2;6;{row}M\x1b[<2;6;{row}m")
    );

    r.move_to(on);
    r.app
        .on_mouse_wheel(r.wid, MouseScrollDelta::LineDelta(0.0, 1.0));
    r.click(MouseButton::Right);
    assert_eq!(r.sent(), "", "over an instrument, the sill takes them");
}

/// View ▸ Stats Bars' checkmark is the applied form: each config apply
/// republishes it, and so does the wait's menu publish.
#[test]
fn the_stats_bars_checkmark_follows_the_applied_form() {
    let mut app = App::headless_for_test();
    let dir = std::env::temp_dir().join(format!("aterm-stats-bars-menu-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let checked = || {
        [
            MenuAction::StatsBarsDeck,
            MenuAction::StatsBarsSill,
            MenuAction::StatsBarsOff,
        ]
        .map(native_menu_checked)
    };
    crate::app_colorscheme::install_resolved_colors::apply_config_text(
        &mut app,
        &dir,
        "[sys]\nstats_bars = \"sill\"\n",
    );
    assert_eq!(app.stats_bars_form(), StatsBars::Sill);
    assert_eq!(checked(), [Some(false), Some(true), Some(false)]);
    crate::app_colorscheme::install_resolved_colors::apply_config_text(&mut app, &dir, "");
    assert_eq!(checked(), [Some(true), Some(false), Some(false)]);

    app.config.sys = Some(crate::app_config::SysConfig {
        stats_bars: Some("off".into()),
    });
    app.publish_menu_live();
    assert_eq!(checked(), [Some(false), Some(false), Some(true)]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Choosing a form with no config lane to write it says so, and keeps the
/// durable form; choosing the form already shown writes and says nothing.
#[test]
fn a_stats_bars_choice_that_cannot_be_saved_says_so() {
    let mut app = App::headless_for_test();
    assert!(
        app.proxy.is_none(),
        "PRECONDITION: no event loop, no config lane"
    );
    app.user_set_stats_bars(StatsBars::Deck);
    assert!(app.messages.live_by_key("sys.stats-bars-save").is_none());
    app.user_set_stats_bars(StatsBars::Sill);
    assert_eq!(
        app.stats_bars_form(),
        StatsBars::Deck,
        "the bars keep the durable form"
    );
    let row = app
        .messages
        .live_by_key("sys.stats-bars-save")
        .expect("the failure is said");
    assert_eq!(row.msg.title, "Couldn't set the stats bars to Sill");
}

/// A change between the sill and off moves no row, column or head: the
/// program sees no resize. The sill's lens closes with the form.
#[test]
fn switching_between_the_sill_and_off_regrids_nothing() {
    let mut r = rig("sill");
    let dir = std::env::temp_dir().join(format!("aterm-stats-bars-regrid-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    crate::app_colorscheme::install_resolved_colors::apply_config_text(
        &mut r.app,
        &dir,
        "[sys]\nstats_bars = \"sill\"\n",
    );
    assert!(r.app.toggle_stats_lens());
    let term = r
        .app
        .pool
        .get(r.session)
        .expect("live session")
        .term
        .clone();
    let grid = |app: &App, wid: WindowId| {
        let ws = &app.windows[&wid];
        (
            ws.rows,
            ws.cols,
            ws.metrics.head,
            ws.metrics.pad,
            ws.metrics.pad_top,
        )
    };
    let before = (grid(&r.app, r.wid), term_lock(&term).resize_lifetime());
    crate::app_colorscheme::install_resolved_colors::apply_config_text(
        &mut r.app,
        &dir,
        "[sys]\nstats_bars = \"off\"\n",
    );
    assert_eq!(r.app.stats_bars_form(), StatsBars::Off);
    assert_eq!(
        (grid(&r.app, r.wid), term_lock(&term).resize_lifetime()),
        before
    );
    assert_eq!(r.lens(), None, "the lens closes with its form");
    let _ = std::fs::remove_dir_all(&dir);
}
