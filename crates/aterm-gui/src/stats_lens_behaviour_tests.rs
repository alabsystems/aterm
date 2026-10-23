// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Input and focus routing tests for the stats lens and sill.

use std::io::{ErrorKind, Read};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aterm_session::sink::SinkWriter;
use aterm_window::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta};
use aterm_window::keyboard::{
    Key, KeyCode, KeyLocation, ModifiersState, NamedKey, PhysicalKey, SmolStr,
};

use super::{LEAVE, Lens, Opened};
use crate::input::{InputEvent, PasteFraming, Source};
use crate::menu::{MenuAction, native_menu_checked};
use crate::stats_deck::StatsBars;
use crate::stats_paint::{Bar, Hit, Instrument};
use crate::stats_plan::DeckForm;
use crate::{App, WindowId, term_lock};

/// A private observer of the session's PTY input.
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

/// One window drawing `form`.
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
    /// Everything the program was sent before a marker pasted to it now.
    fn sent(&mut self) -> String {
        const MARK: &str = "<marker>";
        self.app.input_to_session(
            self.wid,
            InputEvent::Paste(MARK.to_owned(), PasteFraming::Gesture { bracketed: false }),
            Source::Controller,
            Some(self.session),
            crate::app_input::PressPhase::Initial,
        );
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

    /// Turn on the program's mouse reporting (`modes`).
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

    /// The window's Deck in `form`.
    fn deck(&mut self, form: DeckForm, width_pt: Option<f64>) {
        let (cw, cell) = self.app.win_cell_size(self.wid);
        let pad = self.app.win_pad(self.wid);
        let ws = self.app.windows.get_mut(&self.wid).expect("window");
        ws.deck_form = form;
        if let Some(w) = width_pt {
            let px = (w * ws.scale).round() as usize;
            ws.cols = u16::try_from((px - 2 * pad) / cw).expect("a window's columns");
        }
        ws.metrics.head = crate::stats_deck::deck_px(StatsBars::Deck, form, ws.scale, cell);
    }

    /// Rest on the form's first hit `pick` takes until the rest is served.
    fn rest_on_hit(&mut self, pick: impl Fn(&Hit) -> bool) -> Hit {
        let (at, hit) = self.hit(pick);
        self.move_to(at);
        let due = self.app.stats_lens_deadline().expect("the rest's deadline");
        self.app.service_stats_lens(due);
        hit
    }

    /// The frame point at the centre of the sill's first `want` instrument.
    fn instrument(&self, want: Instrument) -> ((f64, f64), Hit) {
        let f = self.app.deck_frame(self.wid).expect("the form shows");
        assert_eq!(f.form, StatsBars::Sill);
        self.hit(|h| h.instrument() == want)
    }

    /// The frame point at the centre of the form's first hit `pick` takes.
    fn hit(&self, pick: impl Fn(&Hit) -> bool) -> ((f64, f64), Hit) {
        let _ = self.app.stats_deck_fp(self.wid);
        let f = self.app.deck_frame(self.wid).expect("the form shows");
        let hits = self.app.windows[&self.wid].stats_hits.borrow().clone();
        let hit = hits
            .iter()
            .find(|h| pick(h))
            .copied()
            .unwrap_or_else(|| panic!("the form has the hit: {hits:?}"));
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

    /// ⌘ and `c`, as the keyboard sends them.
    fn command(&mut self, code: KeyCode, c: &str) {
        self.chord(ModifiersState::SUPER, code, c);
    }

    /// `mods` held over `c`.
    fn chord(&mut self, mods: ModifiersState, code: KeyCode, c: &str) {
        self.app.windows.get_mut(&self.wid).expect("window").mods = mods;
        self.key(code, Key::Character(SmolStr::new(c)), Some(c));
        self.app.windows.get_mut(&self.wid).expect("window").mods = ModifiersState::empty();
    }

    /// What the test build's clipboard holds.
    fn clipboard(&self) -> Option<String> {
        crate::control::PBPASTE_STUB.with(|b| b.borrow().clone())
    }

    /// The sentence ⌘C copies for `focus`, said for the window's front tab.
    fn sentence(&self, focus: Instrument) -> String {
        let snap = self.app.vitals.snapshot.clone().expect("a snapshot");
        let (tab_key, _) = self.app.deck_tab(self.wid);
        let loc = aterm_vitals::words::Locale {
            platform: aterm_vitals::words::Platform::host(),
            utc_offset_s: crate::presence::local_offset_s().unwrap_or(0),
        };
        super::lens_sentence(&snap, focus, tab_key, &loc)
    }
}

/// A pointer resting on an instrument opens the hover lens there when the
/// rest is served, not before; a pointer passing over opens none.
#[test]
fn a_rest_opens_the_hover_lens_and_passing_over_opens_none() {
    let mut r = rig("sill");
    let (on, hit) = r.instrument(Instrument::Cpu);
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
            focus: hit.instrument(),
            bar: hit.bar(),
            how: Opened::Hover,
        })
    );
}

/// A hover lens leaves with the pointer once its leave runs out, and stays
/// while the pointer is over the lens itself.
#[test]
fn a_hover_lens_leaves_with_the_pointer_and_stays_while_over_the_lens() {
    let mut r = rig("sill");
    r.rest_on(Instrument::Cpu);
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
    let (on, hit) = r.instrument(Instrument::Cpu);

    r.move_to(on);
    r.click(MouseButton::Left);
    assert_eq!(
        r.lens(),
        Some(Lens {
            focus: hit.instrument(),
            bar: hit.bar(),
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
/// modifier is inert; any other key closes it and reaches the terminal. Once
/// Return has pinned it, it still takes the arrows.
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
    assert_eq!(r.lens(), at(Instrument::Cpu, Bar::Global));
    r.named(KeyCode::ArrowRight, NamedKey::ArrowRight);
    assert_eq!(r.lens(), at(Instrument::Gpu, Bar::Global));
    r.named(KeyCode::ArrowLeft, NamedKey::ArrowLeft);
    r.named(KeyCode::ArrowLeft, NamedKey::ArrowLeft);
    assert_eq!(
        r.lens(),
        at(Instrument::Processes, Bar::Global),
        "the focus wraps"
    );
    r.named(KeyCode::ArrowDown, NamedKey::ArrowDown);
    assert_eq!(r.lens(), at(Instrument::Processes, Bar::Aterm));
    r.named(KeyCode::ArrowUp, NamedKey::ArrowUp);
    r.named(KeyCode::ArrowUp, NamedKey::ArrowUp);
    assert_eq!(
        r.lens(),
        at(Instrument::Processes, Bar::Tab),
        "the bar wraps"
    );
    r.key(
        KeyCode::ShiftLeft,
        Key::Named(NamedKey::Shift),
        NamedKey::Shift.to_text(),
    );
    assert_eq!(
        r.lens(),
        at(Instrument::Processes, Bar::Tab),
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
    assert_eq!(
        r.lens(),
        Some(Lens {
            focus: Instrument::Cpu,
            bar: Bar::Tab,
            how: Opened::Pinned,
        }),
        "a pinned lens takes the arrows (⌘I reopened Processes, Tab)"
    );
    assert_eq!(r.sent(), "", "and they reach no program");
    r.key(KeyCode::KeyX, Key::Character(SmolStr::new("x")), Some("x"));
    assert_eq!(r.lens(), None, "another key closes the pinned lens");
    assert_eq!(r.sent(), "x", "and reaches the terminal");
}

/// A lens pinned by a click steps over `Instrument::ALL` with ←/→, and ⌘C
/// (or Edit ▸ Copy, or the chord bound to Copy) copies the instrument's
/// sentence, reaching no program; a hover lens takes no key.
#[test]
fn a_pinned_lens_steps_the_instruments_and_command_c_copies_their_sentence() {
    let mut r = rig("sill");
    let (on, _) = r.instrument(Instrument::Cpu);
    r.move_to(on);
    r.click(MouseButton::Left);
    assert!(r.lens().is_some_and(|l| l.how == Opened::Pinned));
    let mut seen = vec![Instrument::Cpu];
    for _ in 1..Instrument::ALL.len() {
        r.named(KeyCode::ArrowRight, NamedKey::ArrowRight);
        seen.push(r.lens().expect("open").focus);
    }
    assert_eq!(seen, Instrument::ALL, "→ steps over every instrument");
    r.named(KeyCode::ArrowRight, NamedKey::ArrowRight);
    assert_eq!(
        r.lens().map(|l| l.focus),
        Some(Instrument::Cpu),
        "and wraps"
    );
    r.named(KeyCode::ArrowLeft, NamedKey::ArrowLeft);
    assert_eq!(r.lens().map(|l| l.focus), Some(Instrument::Processes));

    r.command(KeyCode::KeyC, "c");
    assert!(r.lens().is_some(), "⌘C keeps the lens");
    assert_eq!(r.clipboard(), Some(r.sentence(Instrument::Processes)));
    assert_eq!(r.sent(), "", "the lens's keys reach no program");
    crate::control::PBPASTE_STUB.with(|b| *b.borrow_mut() = None);
    assert!(r.app.copy_selection_in(r.wid), "Edit ▸ Copy copies it too");
    assert_eq!(r.clipboard(), Some(r.sentence(Instrument::Processes)));
    r.app.keybindings = crate::keybinding::Keybindings::resolved(Some(
        &[("ctrl+shift+c".to_owned(), "copy".to_owned())]
            .into_iter()
            .collect(),
    ));
    crate::control::PBPASTE_STUB.with(|b| *b.borrow_mut() = None);
    r.chord(
        ModifiersState::CONTROL | ModifiersState::SHIFT,
        KeyCode::KeyC,
        "C",
    );
    assert!(r.lens().is_some(), "the Copy chord keeps the lens");
    assert_eq!(r.clipboard(), Some(r.sentence(Instrument::Processes)));
    assert_eq!(r.sent(), "", "and reaches no program");

    r.named(KeyCode::Escape, NamedKey::Escape);
    r.rest_on(Instrument::Cpu);
    r.named(KeyCode::ArrowRight, NamedKey::ArrowRight);
    assert_eq!(r.lens(), None);
    assert_eq!(r.sent(), "\x1b[C");
}

/// ⌘I opens the keyboard lens on CPU at first, then on the last instrument
/// a lens showed, and closes an open one.
#[test]
fn command_i_reopens_the_last_instrument() {
    let mut r = rig("sill");
    assert!(r.app.toggle_stats_lens());
    assert_eq!(
        r.lens().map(|l| (l.focus, l.how)),
        Some((Instrument::Cpu, Opened::Keyboard))
    );
    r.named(KeyCode::ArrowRight, NamedKey::ArrowRight);
    r.named(KeyCode::ArrowDown, NamedKey::ArrowDown);
    assert!(r.app.toggle_stats_lens(), "⌘I closes it");
    assert_eq!(r.lens(), None);
    assert!(r.app.toggle_stats_lens());
    assert_eq!(
        r.lens(),
        Some(Lens {
            focus: Instrument::Gpu,
            bar: Bar::Aterm,
            how: Opened::Keyboard,
        }),
        "it reopens where the last lens was"
    );
}

/// Whenever Disk becomes the lens's instrument, the "where" survey is asked
/// for: opened there, or stepped to.
#[test]
fn the_disk_lens_asks_for_the_where_survey() {
    let mut r = rig("sill");
    assert!(r.app.toggle_stats_lens());
    r.named(KeyCode::ArrowRight, NamedKey::ArrowRight);
    r.named(KeyCode::ArrowRight, NamedKey::ArrowRight);
    assert_eq!(r.lens().map(|l| l.focus), Some(Instrument::Memory));
    assert!(
        !r.app.vitals.survey_asked(),
        "no survey for CPU, GPU or Memory"
    );
    r.named(KeyCode::ArrowRight, NamedKey::ArrowRight);
    assert_eq!(r.lens().map(|l| l.focus), Some(Instrument::Disk));
    assert!(r.app.vitals.survey_asked(), "Disk asks for the survey");

    let mut r = rig("sill");
    r.app
        .open_stats_lens(r.wid, Instrument::Disk, Bar::Global, Opened::Pinned);
    assert_eq!(r.lens().map(|l| l.focus), Some(Instrument::Disk));
    assert!(
        r.app.vitals.survey_asked(),
        "a Disk lens opened on Disk asks too"
    );
}

/// A hover lens moves to another instrument only after a rest there; a
/// pinned lens stays where it was put.
#[test]
fn a_hover_lens_moves_after_a_rest_and_a_pinned_lens_stays() {
    let mut r = rig("sill");
    r.rest_on(Instrument::Cpu);
    let (gpu, hit) = r.instrument(Instrument::Gpu);
    r.move_to(gpu);
    assert_eq!(
        r.lens().map(|l| l.focus),
        Some(Instrument::Cpu),
        "not before a rest"
    );
    let due = r.app.stats_lens_deadline().expect("a rest is armed");
    r.app.service_stats_lens(due);
    assert_eq!(
        r.lens(),
        Some(Lens {
            focus: hit.instrument(),
            bar: hit.bar(),
            how: Opened::Hover,
        })
    );

    let mut r = rig("sill");
    let (cpu, _) = r.instrument(Instrument::Cpu);
    r.move_to(cpu);
    r.click(MouseButton::Left);
    let (gpu, _) = r.instrument(Instrument::Gpu);
    r.move_to(gpu);
    assert_eq!(
        r.app.stats_lens_deadline(),
        None,
        "a pinned lens arms no rest"
    );
    assert_eq!(r.lens().map(|l| l.focus), Some(Instrument::Cpu));
}

/// T-G10 on the Deck, through the App's own routing: a rest on a Full row
/// opens its hover lens on the machine, a rest on a Folded scope cell opens
/// it on that cell's scope and a click there pins it, and a click on an
/// fslim block's state label pins the lens on that state's instrument.
#[test]
fn the_decks_rows_cells_and_state_labels_open_their_lens() {
    use crate::stats_paint::HitKind;
    let mut r = rig("deck");
    r.deck(DeckForm::Full, None);
    r.rest_on_hit(|h| h.kind == HitKind::Row(Instrument::Memory, Bar::Global));
    assert_eq!(
        r.lens(),
        Some(Lens {
            focus: Instrument::Memory,
            bar: Bar::Global,
            how: Opened::Hover,
        }),
        "a rest on a Full row"
    );
    r.named(KeyCode::Escape, NamedKey::Escape);
    assert_eq!(r.lens(), None);
    assert_eq!(
        r.sent(),
        "\x1b",
        "a hover lens's Esc goes on to the program"
    );

    r.deck(DeckForm::Folded, Some(1200.0));
    r.rest_on_hit(|h| h.kind == HitKind::Row(Instrument::Disk, Bar::Tab));
    assert_eq!(
        r.lens(),
        Some(Lens {
            focus: Instrument::Disk,
            bar: Bar::Tab,
            how: Opened::Hover,
        }),
        "a rest on a Folded scope cell"
    );
    r.click(MouseButton::Left);
    assert_eq!(
        r.lens(),
        Some(Lens {
            focus: Instrument::Disk,
            bar: Bar::Tab,
            how: Opened::Pinned,
        }),
        "a click pins it"
    );
    r.named(KeyCode::Escape, NamedKey::Escape);
    assert_eq!(r.lens(), None);

    let trouble =
        crate::stats_deck::paint_tests::scene(crate::stats_deck::paint_tests::Scene::Trouble);
    r.app.vitals.snapshot = Some(Arc::new(trouble.clone()));
    r.deck(DeckForm::Folded, Some(350.0));
    let (at, hit) = r.hit(|h| matches!(h.kind, HitKind::Row(_, Bar::Global)) && h.rect.1 < 1.0);
    let HitKind::Row(focus, _) = hit.kind else {
        unreachable!("a row's hit")
    };
    let res = focus.resource().expect("a row's resource");
    let loc = aterm_vitals::words::Locale {
        platform: aterm_vitals::words::Platform::host(),
        utc_offset_s: 0,
    };
    assert!(
        aterm_vitals::words::state(res, &trouble, &loc).is_some(),
        "the label names {res:?}'s state"
    );
    r.move_to(at);
    r.click(MouseButton::Left);
    assert_eq!(
        r.lens(),
        Some(Lens {
            focus,
            bar: Bar::Global,
            how: Opened::Pinned,
        }),
        "a click on the state label"
    );
    assert_eq!(r.sent(), "", "the program sees none of the rest");
}

/// On the Deck, a rest on the header opens How to read.
#[test]
fn a_rest_on_the_decks_header_opens_how_to_read() {
    let mut r = rig("deck");
    let (_, cell) = r.app.win_cell_size(r.wid);
    let ws = r.app.windows.get_mut(&r.wid).expect("window");
    let deck = crate::stats_deck::deck_px(StatsBars::Deck, ws.deck_form, ws.scale, cell);
    ws.metrics.head = deck;
    let (on, _) = r.hit(|h| h.kind == crate::stats_paint::HitKind::Header);
    r.move_to(on);
    let due = r.app.stats_lens_deadline().expect("the rest's deadline");
    r.app.service_stats_lens(due);
    assert_eq!(
        r.lens(),
        Some(Lens {
            focus: Instrument::HowToRead,
            bar: Bar::Global,
            how: Opened::Hover,
        })
    );
}

/// T-G12 through the App (Stats Deck v2 §12): the window's tree holds the
/// deck's group, six meters each said in its row's sentence for the
/// window's front tab and a group of one meter a core; an open lens is a
/// group named "<Name> close-up" whose wells are named by their headings,
/// the first well of a lens that takes the keys taking focus (the keyboard
/// lens's, and still once Return has pinned it), and How to read is read
/// whole.
#[cfg(a11y_tree)]
#[test]
fn the_tree_reads_the_deck_and_its_lens() {
    let mut r = rig("deck");
    let (_, cell) = r.app.win_cell_size(r.wid);
    let ws = r.app.windows.get_mut(&r.wid).expect("window");
    ws.metrics.head = crate::stats_deck::deck_px(StatsBars::Deck, ws.deck_form, ws.scale, cell);

    let stats = r.app.stats_a11y(r.wid).expect("the deck is read");
    let group = &stats.group;
    assert_eq!(group.name, "Stats deck, last 10 minutes");
    let names: Vec<&str> = group.meters.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(
        names,
        ["CPU", "GPU", "Memory", "Disk", "Network", "Processes"]
    );
    assert_eq!(group.meters[0].words, r.sentence(Instrument::Cpu));
    assert_eq!(group.meters[2].words, r.sentence(Instrument::Memory));
    assert_eq!(group.cores_name, "10 CPU cores");
    assert_eq!(group.cores.len(), 10);
    assert!(stats.lens.is_none(), "no lens is open");

    r.app
        .open_stats_lens(r.wid, Instrument::Cpu, Bar::Global, Opened::Keyboard);
    let lens = r
        .app
        .stats_a11y(r.wid)
        .and_then(|s| s.lens)
        .expect("the lens is read");
    assert_eq!(lens.name, "CPU close-up");
    assert!(lens.keys);
    assert_eq!(
        lens.wells.first().map(|(name, _)| name.as_str()),
        Some("Now")
    );
    assert!(
        lens.wells
            .iter()
            .all(|(name, words)| !name.is_empty() && !words.is_empty())
    );

    r.app
        .open_stats_lens(r.wid, Instrument::HowToRead, Bar::Global, Opened::Pinned);
    let lens = r
        .app
        .stats_a11y(r.wid)
        .and_then(|s| s.lens)
        .expect("the lens is read");
    assert_eq!(lens.name, "How to read close-up");
    assert!(lens.keys);
    assert_eq!(
        lens.explainer,
        super::how_to_read_words(aterm_vitals::words::Platform::host())
    );

    let _ = r.app.close_stats_lens(r.wid);
    assert!(r.app.toggle_stats_lens());
    r.named(KeyCode::Enter, NamedKey::Enter);
    assert!(r.lens().is_some_and(|l| l.how == Opened::Pinned));
    let update = r.app.grid_a11y_tree(r.wid, None).expect("the tree");
    let (_, group) = update
        .nodes
        .iter()
        .find(|(_, n)| n.label().is_some_and(|l| l.ends_with("close-up")))
        .expect("the lens's group");
    assert_eq!(
        update.focus,
        group.children()[0],
        "Return keeps the focus in the lens"
    );
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
    let (on, _) = r.instrument(Instrument::Cpu);
    let last_row = r.app.windows[&r.wid].rows - 1;

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
    let scratch = aterm_tempfile::tempdir().expect("scratch dir");
    let dir = scratch.path().to_path_buf();
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
        ..Default::default()
    });
    app.publish_menu_live();
    assert_eq!(checked(), [Some(false), Some(false), Some(true)]);
}

/// View ▸ Deck Size round-trips: each radio row names its choice, its tag
/// and its invoke name; the config's word is the applied choice, the native
/// radio's checkmark and the palette's; choosing one with no config lane to
/// write it says so and keeps the durable choice, and choosing the applied
/// one writes and says nothing.
#[test]
fn the_deck_size_setting_round_trips_through_the_config_and_the_menu() {
    use crate::stats_plan::DeckSizePref;
    let mut app = App::headless_for_test();
    let scratch = aterm_tempfile::tempdir().expect("scratch dir");
    let dir = scratch.path().to_path_buf();
    let rows = [
        MenuAction::DeckSizeAuto,
        MenuAction::DeckSizeFull,
        MenuAction::DeckSizeFolded,
    ];
    let palette = |app: &App| {
        let mut state = crate::palette::PaletteState::new();
        state.resolve(&app.palette_live());
        rows.map(|action| {
            state
                .rows()
                .iter()
                .find(|row| row.action == action)
                .unwrap_or_else(|| panic!("{action:?} has a palette row"))
                .checked
        })
    };
    for (row, pref) in rows.into_iter().zip(DeckSizePref::ALL) {
        assert_eq!(row.deck_size_pref(), Some(pref));
        assert_eq!(MenuAction::from_tag(row.tag()), Some(row));
        assert_eq!(MenuAction::from_invoke_name(&format!("{row:?}")), Some(row));
        let text = format!("[sys]\nstats_deck_size = \"{}\"\n", pref.word());
        crate::app_colorscheme::install_resolved_colors::apply_config_text(&mut app, &dir, &text);
        assert_eq!(app.deck_size_pref(), pref, "{text}");
        let want = rows.map(|r| Some(r == row));
        assert_eq!(rows.map(native_menu_checked), want, "{text}");
        assert_eq!(palette(&app), want, "{text}");
    }
    crate::app_colorscheme::install_resolved_colors::apply_config_text(&mut app, &dir, "");
    assert_eq!(app.deck_size_pref(), DeckSizePref::Auto);
    assert_eq!(
        rows.map(native_menu_checked),
        [Some(true), Some(false), Some(false)]
    );

    assert!(
        app.proxy.is_none(),
        "PRECONDITION: no event loop, no config lane"
    );
    app.user_set_deck_size(DeckSizePref::Auto);
    assert!(app.messages.live_by_key("sys.deck-size-save").is_none());
    app.user_set_deck_size(DeckSizePref::Full);
    assert_eq!(
        app.deck_size_pref(),
        DeckSizePref::Auto,
        "the windows keep the durable choice"
    );
    let row = app
        .messages
        .live_by_key("sys.deck-size-save")
        .expect("the failure is said");
    assert_eq!(row.msg.title, "Couldn't set the stats deck's size to Full");
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
    let scratch = aterm_tempfile::tempdir().expect("scratch dir");
    let dir = scratch.path().to_path_buf();
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
}
