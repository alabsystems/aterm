// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The echo proof over real terminals: what a press admits and what a frame proves of it.

use std::time::{Duration, Instant};

use aterm_core::terminal::Terminal;
use aterm_effects::aos_momentum::{MomentumCode, MomentumHost, RAW_KEY};
use aterm_session::sink::SinkWriter;

use super::super::AosWindow;
use super::{
    ECHO_PROOF_BOUND, Echo, EchoView, HOLD_CAP, KeyRoute, Press, PressRow, admit_key, echo_verdict,
    next_site, typed_echo_cells, unseen_verdict,
};

/// A terminal at a prompt with no shell integration, a recording attached, and the window's hold.
struct Fixture {
    t: Terminal,
    aos: AosWindow,
    host: MomentumHost,
    sink: SinkWriter,
    t0: Instant,
}

const SESSION: u64 = 7;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

impl Fixture {
    fn new(rows: u16) -> Self {
        let mut t = Terminal::new(rows, 40);
        t.process(b"$ ");
        let t0 = Instant::now();
        let mut host = MomentumHost::default();
        host.aos_attach(t0, MomentumCode::Shipped, "test", 1);
        Self {
            t,
            aos: AosWindow::default(),
            host,
            sink: SinkWriter::new(-1),
            t0,
        }
    }

    /// What a press of `typed` at `at` saw.
    fn press_of(&self, at: Instant) -> Press {
        let c = self.t.cursor();
        Press {
            session: SESSION,
            cursor: (c.row, c.col),
            pending_wrap: self.t.grid().pending_wrap(),
            coord_gen: self.t.content_scroll_state().coordinate_generation(),
            alt: self.t.is_alternate_screen(),
            display_offset: self.t.grid().display_offset(),
            print_seq: self.t.print_seq(),
            epoch: self.sink.input_epoch(),
            at,
            row: PressRow::read(&self.t, next_site(&self.aos)),
        }
    }

    /// Types `ch` as a raw key whose press saw `press`.
    fn admit(&mut self, ch: char, press: &Press) -> KeyRoute {
        self.admit_as(ch, press, false)
    }

    /// Types `ch` as a raw key whose press saw `press`, under the person's grant when `covered`.
    fn admit_as(&mut self, ch: char, press: &Press, covered: bool) -> KeyRoute {
        admit_key(
            &mut self.aos,
            &mut self.host,
            Some(RAW_KEY),
            press,
            |cells| typed_echo_cells(Some(ch), None, false, cells),
            covered,
        )
    }

    /// Types `text`'s chars as raw keys, `at` and a millisecond apart.
    fn type_raw(&mut self, text: &str, at: Instant) -> Vec<KeyRoute> {
        self.type_as(text, at, false)
    }

    /// [`Self::type_raw`] under the person's grant.
    fn type_granted(&mut self, text: &str, at: Instant) -> Vec<KeyRoute> {
        self.type_as(text, at, true)
    }

    fn type_as(&mut self, text: &str, at: Instant, covered: bool) -> Vec<KeyRoute> {
        text.chars()
            .enumerate()
            .map(|(i, ch)| {
                let press = self.press_of(at + ms(u64::try_from(i).expect("short")));
                self.admit_as(ch, &press, covered)
            })
            .collect()
    }

    /// What a frame at `now` proves of the open hold the person's grant opened.
    fn unseen_at(&self, now: Instant) -> Echo {
        let hold = self.aos.hold.as_ref().expect("a hold is open");
        let mut view = EchoView::default();
        view.read(&self.t, SESSION, hold);
        unseen_verdict(hold, &view, now)
    }

    /// What a frame at `now` that read the terminal as `session` proves.
    fn verdict_as(&self, session: u64, now: Instant) -> Echo {
        let hold = self.aos.hold.as_ref().expect("a hold is open");
        let mut view = EchoView::default();
        view.read(&self.t, session, hold);
        echo_verdict(hold, &view, now)
    }

    /// Makes the open hold continue a proven run `run` that ended at its start.
    fn continue_run(&mut self, run: &str) {
        let hold = self.aos.hold.as_mut().expect("held");
        hold.run = run.chars().collect();
        hold.keys[0].joins = true;
    }

    fn verdict(&self, now: Instant) -> Echo {
        self.verdict_as(SESSION, now)
    }
}

/// `ab` typed raw at a prompt, then the terminal fed `before` (the program's output), read at
/// `+5 ms`.
fn ab_then(before: &[u8]) -> Echo {
    let mut f = Fixture::new(4);
    let at = f.t0 + ms(10);
    assert_eq!(f.type_raw("ab", at), [KeyRoute::Hold, KeyRoute::Hold]);
    f.t.process(before);
    f.verdict(at + ms(5))
}

#[test]
fn echo_verdict_releases_only_a_corroborated_visible_run_printed_in_place() {
    assert_eq!(
        ab_then(b"ab"),
        Echo::Release(2),
        "the program's own echo, in place"
    );
    assert_eq!(
        ab_then(b"a"),
        Echo::Pending,
        "half shown: a lone first key waits for its twin"
    );

    for (twin, fed, want) in [
        ("a mask glyph for the first char", &b"*b"[..], Echo::Refute),
        ("a mask glyph for the second", b"a*", Echo::Refute),
        ("pwfeedback's two masks", b"**", Echo::Refute),
        ("another char, a torn redraw", b"xb", Echo::Pending),
        ("the cursor on the next row", b"ab\r\n", Echo::Pending),
        (
            "the cursor moved, nothing printed",
            b"\x1b[2C",
            Echo::Pending,
        ),
        ("SGR 8, concealed", b"\x1b[8mab", Echo::Pending),
        ("blue on blue", b"\x1b[34;44mab", Echo::Pending),
        (
            "near-black on palette black",
            b"\x1b[38;2;1;1;1mab",
            Echo::Pending,
        ),
        ("inverse and concealed", b"\x1b[7;8mab", Echo::Pending),
        (
            "a combining mark on the echo",
            "a\u{301}b".as_bytes(),
            Echo::Pending,
        ),
        ("an alt-screen swap", b"\x1b[?1049hab", Echo::Refute),
    ] {
        assert_eq!(ab_then(fed), want, "{twin}");
    }

    // Identical keys never corroborate, even echoed exactly.
    let mut f = Fixture::new(4);
    let at = f.t0 + ms(10);
    let _ = f.type_raw("aa", at);
    f.t.process(b"aa");
    assert_eq!(f.verdict(at + ms(5)), Echo::Pending, "identical 'aa'");
    assert_eq!(
        f.verdict(at + ECHO_PROOF_BOUND),
        Echo::Refute,
        "and refuted at the bound"
    );

    // Another session's screen, though it shows the run.
    let mut f = Fixture::new(4);
    let _ = f.type_raw("ab", f.t0);
    f.t.process(b"ab");
    assert_eq!(
        f.verdict_as(SESSION + 1, f.t0),
        Echo::Refute,
        "another session"
    );

    // A content scroll moves every row's coordinates.
    let mut f = Fixture::new(2);
    f.t.process(b"\r\n$ ");
    let _ = f.type_raw("ab", f.t0);
    f.t.process(b"ab\n");
    assert_eq!(
        f.verdict(f.t0),
        Echo::Refute,
        "the screen scrolled under the hold"
    );

    // A scrolled-back viewport proves nothing, whatever history shows at the hold's row.
    let mut f = Fixture::new(3);
    f.t.process(b"\r\n$ ab\r\n\r\n$ ");
    let _ = f.type_raw("ab", f.t0);
    f.t.scroll_display(1);
    assert!(
        f.t.grid().display_offset() > 0,
        "the viewport is scrolled back"
    );
    f.t.process(b"**");
    assert_eq!(
        f.verdict(f.t0),
        Echo::Pending,
        "masks under a scrolled viewport"
    );
    f.t.scroll_display(-1);
    assert_eq!(
        f.verdict(f.t0),
        Echo::Refute,
        "back at the live screen the masks refute"
    );
}

#[test]
fn a_key_typed_over_its_own_glyph_is_refused_at_its_press() {
    let refused = KeyRoute::Private(RAW_KEY);
    // The row already shows `ab` where the keys go; the cursor then moves past it and something
    // prints elsewhere, though the program never echoed a key.
    let typed_over = |f: &mut Fixture| {
        f.t.process(b"\x1b[2C\x1b7\x1b[3;1Hzz\x1b8");
    };
    let mut f = Fixture::new(4);
    f.t.process(b"ab\x1b[1;3H");
    assert_eq!(f.type_raw("a", f.t0), [refused], "'a' over its own 'a'");
    assert!(f.aos.hold.is_none() && !f.host.aos_holding());

    // Negative control: the same keys with the press's read of the row blanked open a hold, and
    // the cursor moving past text that was there all along then proves them.
    let mut f = Fixture::new(4);
    f.t.process(b"ab\x1b[1;3H");
    for (i, ch) in ['a', 'b'].into_iter().enumerate() {
        let mut press = f.press_of(f.t0 + ms(u64::try_from(i).expect("two")));
        press.row.chars = [' '; HOLD_CAP];
        assert_eq!(f.admit(ch, &press), KeyRoute::Hold);
    }
    typed_over(&mut f);
    assert_eq!(
        f.verdict(f.t0 + ms(5)),
        Echo::Release(2),
        "negative control: the press's read is what refuses it"
    );

    let mut f = Fixture::new(4);
    f.t.process(b" b\x1b[1;3H");
    assert_eq!(
        f.type_raw("ab", f.t0),
        [KeyRoute::Hold, refused],
        "a later key over its own glyph, at the hold's next column"
    );

    for (what, on_row, typed) in [
        ("other text, as a redraw overwrites", &b"xy"[..], "ab"),
        ("a typed blank over a blank", b"", "a b"),
    ] {
        let mut f = Fixture::new(4);
        f.t.process(on_row);
        f.t.process(b"\x1b[1;3H");
        assert!(
            f.type_raw(typed, f.t0).iter().all(|r| *r == KeyRoute::Hold),
            "{what}"
        );
    }

    let mut f = Fixture::new(4);
    let mut press = f.press_of(f.t0);
    press.row = PressRow::NONE;
    assert_eq!(f.admit('a', &press), refused, "a press that read no row");
    let _ = f.type_raw("a", f.t0);
    let mut press = f.press_of(f.t0);
    press.row = PressRow::read(&f.t, None);
    assert_eq!(
        f.admit('b', &press),
        refused,
        "a row read at the cursor, not at the hold's next column"
    );
}

#[test]
fn a_proof_is_bounded_from_the_first_key_and_a_late_one_releases_at_its_frame() {
    let mut f = Fixture::new(4);
    let at = f.t0 + ms(10);
    let _ = f.type_raw("ab", at);
    assert_eq!(
        f.verdict(at + ECHO_PROOF_BOUND - ms(1)),
        Echo::Pending,
        "just under"
    );
    assert_eq!(
        f.verdict(at + ECHO_PROOF_BOUND),
        Echo::Refute,
        "at the bound"
    );
    f.t.process(b"ab");
    assert_eq!(
        f.verdict(at + ECHO_PROOF_BOUND + ms(50)),
        Echo::Release(2),
        "a proof first shown after the bound still releases"
    );
}

#[test]
fn a_run_continues_only_on_a_glyph_that_is_neither_a_blank_nor_a_mask() {
    // A blank the program never printed: the cursor steps past it (CSI C) and a status line is
    // redrawn elsewhere, so the cell reads ' ' and the terminal printed since the press.
    let stepped_past = &b"\x1b[C\x1b7\x1b[3;1Hzz\x1b8"[..];
    for (typed, shown, want) in [
        ("c", &b"c"[..], Echo::Release(1)),
        ("*", b"*", Echo::Pending),
        ("c", b"\x1b[1;3HaXc", Echo::Pending),
        (" ", stepped_past, Echo::Pending),
        (" ", b" ", Echo::Pending),
        (" c", b" c", Echo::Release(2)),
    ] {
        let mut f = Fixture::new(4);
        f.t.process(b"ab");
        let _ = f.type_raw(typed, f.t0);
        f.continue_run("ab");
        f.t.process(shown);
        assert_eq!(
            f.verdict(f.t0),
            want,
            "{typed:?} after the run 'ab', shown {shown:?}"
        );
        if want == Echo::Pending {
            assert_eq!(
                f.verdict(f.t0 + ECHO_PROOF_BOUND),
                Echo::Refute,
                "{typed:?} shown {shown:?}: unproven at the bound"
            );
        }
    }
    for (what, between, want) in [
        ("no input between", false, Echo::Release(2)),
        ("a reply between", true, Echo::Pending),
    ] {
        let mut f = Fixture::new(4);
        f.t.process(b"ab");
        let _ = f.type_raw("c", f.t0);
        f.continue_run("ab");
        if between {
            let _ = f.sink.write_reply(b"\x1b[I");
        }
        let _ = f.type_raw("c", f.t0);
        f.t.process(b"cc");
        assert_eq!(f.verdict(f.t0), want, "'cc' continuing 'ab', {what}");
    }
}

#[test]
fn a_press_off_the_live_screen_without_cells_or_past_the_cap_is_refused() {
    let refused = KeyRoute::Private(RAW_KEY);
    let raw = |f: &mut Fixture, typed: Option<char>, ime: Option<&str>| {
        let press = f.press_of(f.t0);
        admit_key(
            &mut f.aos,
            &mut f.host,
            Some(RAW_KEY),
            &press,
            |cells| typed_echo_cells(typed, ime, false, cells),
            false,
        )
    };
    let mut f = Fixture::new(4);
    assert_eq!(raw(&mut f, Some('\r'), None), refused, "Enter has no cells");
    assert_eq!(
        raw(&mut f, Some('\u{301}'), None),
        refused,
        "a lone combining mark"
    );
    assert_eq!(
        raw(&mut f, None, Some("e\u{301}")),
        refused,
        "a decomposed commit"
    );
    assert!(f.aos.hold.is_none() && !f.host.aos_holding());
    assert_eq!(
        raw(&mut f, None, Some("日本")),
        KeyRoute::Hold,
        "an IME commit, wide"
    );
    assert_eq!(
        f.aos.hold.as_ref().expect("held").cells,
        ['日', '\0', '本', '\0'],
        "each wide glyph's continuation column"
    );

    let mut f = Fixture::new(4);
    f.t.process(&[b'x'; 38]);
    assert!(f.t.grid().pending_wrap(), "the cursor waits at the margin");
    assert_eq!(raw(&mut f, Some('a'), None), refused, "a deferred wrap");

    let mut f = Fixture::new(3);
    f.t.process(b"\r\n\r\n\r\n$ ");
    f.t.scroll_display(1);
    assert_eq!(
        raw(&mut f, Some('a'), None),
        refused,
        "a scrolled-back viewport"
    );

    let mut f = Fixture::new(4);
    f.host.aos_detach();
    assert_eq!(
        raw(&mut f, Some('a'), None),
        refused,
        "no recording to hold"
    );

    let mut f = Fixture::new(4);
    let typed: String = ('a'..='z').chain('A'..='F').collect();
    assert_eq!(typed.len(), HOLD_CAP);
    assert!(
        f.type_raw(&typed, f.t0)
            .iter()
            .all(|r| *r == KeyRoute::Hold)
    );
    assert_eq!(f.type_raw("G", f.t0), [refused], "one column past the cap");

    let mut f = Fixture::new(4);
    let _ = f.type_raw("a", f.t0);
    f.t.process(b"\x1b[?1049h");
    assert_eq!(
        f.type_raw("b", f.t0),
        [refused],
        "another screen than the hold's"
    );

    let mut f = Fixture::new(4);
    let press = f.press_of(f.t0);
    let echoed = admit_key(
        &mut f.aos,
        &mut f.host,
        None,
        &press,
        |_| unreachable!("an echoed key with no hold reads no cells"),
        false,
    );
    assert_eq!(echoed, KeyRoute::Record);
    let _ = f.type_raw("a", f.t0);
    let press = f.press_of(f.t0);
    let joined = admit_key(
        &mut f.aos,
        &mut f.host,
        None,
        &press,
        |cells| typed_echo_cells(Some('b'), None, false, cells),
        false,
    );
    assert_eq!(
        joined,
        KeyRoute::Hold,
        "an echoed key joins the open hold behind it"
    );
}

#[test]
fn no_debug_form_carries_a_held_char() {
    for covered in [false, true] {
        let mut f = Fixture::new(4);
        let _ = f.type_as("Zq", f.t0, covered);
        f.t.process(b"Zq");
        let hold = f.aos.hold.as_ref().expect("held");
        let mut view = EchoView::default();
        view.read(&f.t, SESSION, hold);
        for shown in [
            format!("{hold:?}"),
            format!("{view:?}"),
            format!("{:?}", f.aos),
        ] {
            assert!(
                !shown.contains('Z') && !shown.contains('q'),
                "a held char escaped (granted: {covered}): {shown}"
            );
        }
    }
}

/// A raw key with no cells, as the press path routes it.
fn blind(f: &mut Fixture, typed: Option<char>, ime: Option<&str>, covered: bool) -> KeyRoute {
    let press = f.press_of(f.t0);
    admit_key(
        &mut f.aos,
        &mut f.host,
        Some(RAW_KEY),
        &press,
        |cells| typed_echo_cells(typed, ime, false, cells),
        covered,
    )
}

#[test]
fn under_the_grant_only_a_key_with_no_cells_is_held_blind() {
    let refused = KeyRoute::Private(RAW_KEY);
    let mut f = Fixture::new(4);
    assert_eq!(
        blind(&mut f, Some('\r'), None, true),
        KeyRoute::Hold,
        "Enter"
    );
    assert_eq!(
        blind(&mut f, Some('\u{301}'), None, true),
        KeyRoute::Hold,
        "a lone combining mark joins it"
    );
    let hold = f.aos.hold.as_ref().expect("held");
    assert!(
        hold.unseen_ok && hold.blind == 2 && hold.cells.is_empty() && hold.keys.is_empty(),
        "{hold:?}"
    );
    assert!(f.host.aos_holding(), "the host holds for the blind keys");
    assert_eq!(
        f.type_granted("a", f.t0),
        [KeyRoute::Hold],
        "a key with cells joins the granted hold with them"
    );
    assert_eq!(f.aos.hold.as_ref().expect("held").cells, ['a']);

    let mut f = Fixture::new(4);
    assert_eq!(
        blind(&mut f, Some('\r'), None, false),
        refused,
        "negative control: without the grant a key with no cells stops the recording"
    );
    let _ = f.type_raw("a", f.t0);
    assert_eq!(
        blind(&mut f, Some('\r'), None, true),
        refused,
        "a blind key never joins a hold the grant did not open"
    );

    let mut f = Fixture::new(4);
    let typed: String = ('a'..='z').chain('A'..='F').collect();
    assert!(
        f.type_granted(&typed, f.t0)
            .iter()
            .all(|r| *r == KeyRoute::Hold)
    );
    assert_eq!(
        blind(&mut f, Some('\r'), None, true),
        refused,
        "a blind key past the cap"
    );

    // Text that shows a glyph beside a char with no column is no blind key: a field that masks it
    // shows the mask, and no cell of the hold would read it.
    for (what, ime) in [
        ("an emoji with its variation selector", "\u{2764}\u{fe0f}"),
        ("a letter and its combining accent", "e\u{301}"),
    ] {
        let mut f = Fixture::new(4);
        assert_eq!(blind(&mut f, None, Some(ime), true), refused, "{what}");
        assert!(f.aos.hold.is_none(), "{what}: nothing held");
    }
    let mut f = Fixture::new(4);
    assert_eq!(
        blind(&mut f, None, Some("\u{301}\u{200d}"), true),
        KeyRoute::Hold,
        "negative control: text whose every char takes no column is held blind"
    );
}

#[test]
fn under_the_grant_a_key_with_cells_is_held_with_them_or_stops_the_recording() {
    let refused = KeyRoute::Private(RAW_KEY);

    // Over its own glyph: held with its cells, where a mask can still refute it.
    let mut f = Fixture::new(4);
    f.t.process(b"ab\x1b[1;3H");
    assert_eq!(
        f.type_granted("a", f.t0),
        [KeyRoute::Hold],
        "'a' over its own 'a'"
    );
    assert_eq!(f.aos.hold.as_ref().expect("held").cells, ['a']);
    let mut g = Fixture::new(4);
    g.t.process(b"ab\x1b[1;3H");
    assert_eq!(
        g.type_raw("a", g.t0),
        [refused],
        "negative control: without the grant it stops at its press"
    );

    let mut f = Fixture::new(4);
    f.t.process(&[b'*'; 38]);
    assert!(f.t.grid().pending_wrap(), "the cursor waits at the margin");
    assert_eq!(
        f.type_granted("a", f.t0),
        [refused],
        "a deferred wrap, though a field shows masks"
    );

    let mut f = Fixture::new(3);
    f.t.process(b"\r\n\r\n\r\n$ ");
    f.t.scroll_display(1);
    assert_eq!(
        f.type_granted("a", f.t0),
        [refused],
        "a scrolled-back viewport"
    );

    let mut f = Fixture::new(4);
    let mut press = f.press_of(f.t0);
    press.row = PressRow::NONE;
    assert_eq!(
        f.admit_as('a', &press, true),
        refused,
        "a press that read no row"
    );
    let _ = f.type_granted("a", f.t0);
    let mut press = f.press_of(f.t0);
    press.row = PressRow::read(&f.t, Some((0, 7)));
    assert_eq!(
        f.admit_as('b', &press, true),
        refused,
        "a row read at another column than the hold's next"
    );
    let press = f.press_of(f.t0);
    assert_eq!(
        f.admit_as('b', &press, true),
        KeyRoute::Hold,
        "negative control: read at the hold's next column it joins"
    );

    let mut f = Fixture::new(4);
    let typed: String = ('a'..='z').chain('A'..='F').collect();
    assert!(
        f.type_granted(&typed, f.t0)
            .iter()
            .all(|r| *r == KeyRoute::Hold)
    );
    assert_eq!(
        f.type_granted("G", f.t0),
        [refused],
        "one column past the cap"
    );

    let mut f = Fixture::new(4);
    let _ = f.type_granted("a", f.t0);
    f.t.process(b"\x1b[?1049h");
    assert_eq!(
        f.type_granted("b", f.t0),
        [refused],
        "another screen than the hold's"
    );

    let mut f = Fixture::new(4);
    let _ = f.type_raw("a", f.t0);
    assert_eq!(
        f.type_granted("b", f.t0),
        [refused],
        "a granted key never joins a hold the grant did not open"
    );
    let mut f = Fixture::new(4);
    let _ = f.type_granted("a", f.t0);
    assert_eq!(
        f.type_raw("b", f.t0),
        [refused],
        "nor an ungranted key one it did"
    );
}

#[test]
fn a_granted_hold_logs_unseen_what_no_frame_proves_unless_a_mask_shows_in_its_cells() {
    // Proven keys release as seen, as without the grant.
    let mut f = Fixture::new(4);
    let at = f.t0 + ms(10);
    let _ = f.type_granted("ab", at);
    f.t.process(b"ab");
    assert_eq!(f.unseen_at(at + ms(5)), Echo::Release(2), "a proven pair");

    // Nothing shown: pending to the bound, then unseen.
    let mut f = Fixture::new(4);
    let _ = f.type_granted("ab", at);
    assert_eq!(f.unseen_at(at + ECHO_PROOF_BOUND - ms(1)), Echo::Pending);
    assert_eq!(
        f.unseen_at(at + ECHO_PROOF_BOUND),
        Echo::Unseen,
        "at the bound"
    );
    assert_eq!(
        f.verdict(at + ECHO_PROOF_BOUND),
        Echo::Refute,
        "negative control: the same hold without the grant is refuted"
    );

    // A mask in any held cell refutes at once, wherever the cursor is.
    for (what, fed) in [
        ("in place", &b"*b"[..]),
        ("the cursor parked elsewhere", b"a*\x1b[3;1H"),
        ("the cursor back at the start", b"\x1b[1;3H\xe2\x80\xa2"),
    ] {
        let mut f = Fixture::new(4);
        let _ = f.type_granted("ab", at);
        f.t.process(fed);
        assert_eq!(f.unseen_at(at + ms(5)), Echo::Refute, "{what}");
        assert_eq!(
            f.unseen_at(at + ECHO_PROOF_BOUND),
            Echo::Refute,
            "{what}, at the bound"
        );
    }
    let mut f = Fixture::new(4);
    let _ = f.type_granted("*", at);
    f.t.process(b"*");
    assert_eq!(
        f.unseen_at(at + ECHO_PROOF_BOUND),
        Echo::Unseen,
        "a typed mask glyph shown as itself is no mask"
    );

    // A view of another session or a moved screen proves nothing: unseen, unless masked.
    let mut f = Fixture::new(4);
    let _ = f.type_granted("ab", at);
    f.t.process(b"\x1b[?1049h");
    assert_eq!(f.unseen_at(at + ms(5)), Echo::Unseen, "the screen swapped");

    // A blind hold waits to its bound, or until the view moves, and is never released as seen.
    let mut f = Fixture::new(4);
    assert_eq!(blind(&mut f, Some('\r'), None, true), KeyRoute::Hold);
    assert_eq!(f.unseen_at(f.t0 + ECHO_PROOF_BOUND - ms(1)), Echo::Pending);
    assert_eq!(f.unseen_at(f.t0 + ECHO_PROOF_BOUND), Echo::Unseen);
    let _ = f.type_granted("ab", f.t0);
    f.t.process(b"ab");
    assert_eq!(
        f.unseen_at(f.t0 + ms(5)),
        Echo::Pending,
        "a proven pair behind a blind key waits with it"
    );
    f.t.process(b"\x1b[?1049h");
    assert_eq!(f.unseen_at(f.t0 + ms(5)), Echo::Unseen, "the view moved");
}
