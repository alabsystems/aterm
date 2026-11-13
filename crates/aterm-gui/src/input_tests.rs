// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_core::terminal::Terminal;
use aterm_types::mouse::WheelDir;
#[cfg(unix)]
use std::io::Read;
#[cfg(unix)]
use std::os::fd::{FromRawFd, IntoRawFd};
use std::sync::Arc;

#[cfg(unix)]
struct CaptureSink {
    read: i32,
    write: i32,
}

#[cfg(unix)]
impl CaptureSink {
    fn new() -> Self {
        let (read, write) = aterm_uds::pipe().expect("pipe");
        Self {
            read: read.into_raw_fd(),
            write: write.into_raw_fd(),
        }
    }

    fn master(&self) -> i32 {
        self.write
    }

    fn drain(&mut self) -> Vec<u8> {
        if self.write >= 0 {
            unsafe { libc::close(self.write) };
            self.write = -1;
        }
        let mut buf = Vec::new();
        let mut reader = unsafe { std::fs::File::from_raw_fd(self.read) };
        self.read = -1;
        reader.read_to_end(&mut buf).expect("read pipe");
        buf
    }
}

#[cfg(unix)]
impl Drop for CaptureSink {
    fn drop(&mut self) {
        for fd in [self.read, self.write] {
            if fd >= 0 {
                unsafe { libc::close(fd) };
            }
        }
    }
}

#[cfg(windows)]
struct CaptureSink {
    master: i32,
    read: isize,
    stop: isize,
}

#[cfg(windows)]
impl CaptureSink {
    const PIPE_BYTES: u32 = 256 * 1024;

    fn new() -> Self {
        let (mut read, mut write): (isize, isize) = (0, 0);
        // SAFETY: two out-params, default security attributes, explicit size.
        let ok = unsafe {
            winapi::CreatePipe(
                &mut read,
                &mut write,
                std::ptr::null_mut(),
                Self::PIPE_BYTES,
            )
        };
        assert_ne!(ok, 0, "CreatePipe for the egress capture");
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nth = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let name: Vec<u16> = format!("aterm-egress-capture-{}-{nth}\0", std::process::id())
            .encode_utf16()
            .collect();
        // SAFETY: NULL attrs; manual-reset, initially unsignaled; NUL-terminated wide name. The second call opens the SAME object (documented `CreateEventW` behaviour for an existing name).
        let ours = unsafe { winapi::CreateEventW(std::ptr::null_mut(), 1, 0, name.as_ptr()) };
        let theirs = unsafe { winapi::CreateEventW(std::ptr::null_mut(), 1, 0, name.as_ptr()) };
        assert!(ours != 0 && theirs != 0, "CreateEventW for the capture");
        let spawned =
            aterm_pty::adopt_handoff(write, 0, 0, theirs).expect("register the capture session");
        Self {
            master: spawned.master,
            read,
            stop: ours,
        }
    }

    fn master(&self) -> i32 {
        self.master
    }

    fn drain(&mut self) -> Vec<u8> {
        let mut avail: u32 = 0;
        // SAFETY: live read handle; every optional out-param is NULL-allowed.
        let peeked = unsafe {
            winapi::PeekNamedPipe(
                self.read,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut avail,
                std::ptr::null_mut(),
            )
        };
        if peeked == 0 || avail == 0 {
            return Vec::new();
        }
        let mut buf = vec![0u8; avail as usize];
        let mut got: u32 = 0;
        // SAFETY: `buf` holds `avail` writable bytes; synchronous read.
        let ok = unsafe {
            winapi::ReadFile(
                self.read,
                buf.as_mut_ptr(),
                avail,
                &mut got,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(ok, 0, "ReadFile from the egress capture pipe");
        buf.truncate(got as usize);
        buf
    }
}

#[cfg(windows)]
impl Drop for CaptureSink {
    fn drop(&mut self) {
        // SAFETY: handles this struct owns, closed exactly once.
        unsafe { winapi::SetEvent(self.stop) };
        aterm_pty::close_master(self.master);
        unsafe {
            winapi::CloseHandle(self.stop);
            winapi::CloseHandle(self.read);
        }
    }
}

#[cfg(windows)]
mod winapi {
    #![allow(non_snake_case)]

    use std::ffi::c_void;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub(crate) fn CreatePipe(
            read: *mut isize,
            write: *mut isize,
            attrs: *mut c_void,
            size: u32,
        ) -> i32;
        pub(crate) fn CreateEventW(
            attrs: *mut c_void,
            manual_reset: i32,
            initial_state: i32,
            name: *const u16,
        ) -> isize;
        pub(crate) fn SetEvent(event: isize) -> i32;
        pub(crate) fn CloseHandle(handle: isize) -> i32;
        pub(crate) fn PeekNamedPipe(
            pipe: isize,
            buf: *mut u8,
            buf_len: u32,
            read: *mut u32,
            avail: *mut u32,
            left_this_message: *mut u32,
        ) -> i32;
        pub(crate) fn ReadFile(
            file: isize,
            buf: *mut u8,
            want: u32,
            got: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
    }
}

fn egress_bytes(term: &Mutex<Terminal>, ev: &InputEvent) -> Vec<u8> {
    let mut cap = CaptureSink::new();
    let sink = SinkWriter::new(cap.master());
    seam_egress(
        term,
        &crate::mode_mirror_of(term),
        &sink,
        ev,
        EgressMode::Interactive,
    );
    drop(sink);
    cap.drain()
}

#[test]
fn delivered_classifies_short_and_failed_writes() {
    assert_eq!(delivered(Ok(5), 5), Delivery::Full);
    assert_eq!(delivered(Ok(0), 5), Delivery::Failed);
    assert_eq!(delivered(Ok(3), 5), Delivery::Failed);
    assert_eq!(
        delivered(Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe)), 5),
        Delivery::Failed
    );
}

#[test]
fn receipt_tracks_accepted_bytes_not_zero_byte_or_failed_attempts() {
    let term = term_with(&[]);
    let mut cap = CaptureSink::new();
    let sink = SinkWriter::new(cap.master());
    let accepted = seam_egress_receipt(
        &term,
        &crate::mode_mirror_of(&term),
        &sink,
        &InputEvent::KeySequence(b"x".to_vec()),
        EgressMode::Interactive,
    );
    assert!(accepted.accepted_nonempty());
    assert!(accepted.fully_accepted_nonempty());
    drop(sink);
    assert_eq!(cap.drain(), b"x");

    let empty = seam_egress_receipt(
        &term,
        &crate::mode_mirror_of(&term),
        &SinkWriter::new(-1),
        &InputEvent::KeySequence(Vec::new()),
        EgressMode::Interactive,
    );
    assert_eq!(empty.egress, Egress::Reported(Delivery::Full));
    assert!(!empty.accepted_nonempty());
    assert!(!empty.fully_accepted_nonempty());

    let failed = seam_egress_receipt(
        &term,
        &crate::mode_mirror_of(&term),
        &SinkWriter::new(-1),
        &InputEvent::KeySequence(b"x".to_vec()),
        EgressMode::Interactive,
    );
    assert_eq!(failed.egress, Egress::Reported(Delivery::Failed));
    assert!(!failed.accepted_nonempty());
    assert!(!failed.fully_accepted_nonempty());
}

#[test]
fn receipt_preserves_an_earlier_accepted_frame_after_a_later_failure() {
    let mut cap = CaptureSink::new();
    let good = SinkWriter::new(cap.master());
    let bad = SinkWriter::new(-1);
    let mut accepted = Accepted::default();
    assert_eq!(
        emit(&good, EgressMode::Interactive, b"first", &mut accepted),
        Delivery::Full,
    );
    assert!(accepted.direct, "an uncontended whole frame is direct");
    assert_eq!(
        emit(&bad, EgressMode::Interactive, b"second", &mut accepted),
        Delivery::Failed,
    );
    assert!(accepted.order.is_some());
    assert!(
        accepted.direct,
        "a later failed frame accepts nothing and leaves the earlier direct fact alone"
    );
    assert!(
        !accepted.refused,
        "a dead fd is a failure, not a full queue"
    );
    let receipt = EgressReceipt {
        egress: Egress::Reported(Delivery::Failed),
        accepted_order: accepted.order,
        direct: accepted.direct,
        refused: accepted.refused,
        queued: false,
    };
    assert!(receipt.accepted_nonempty());
    assert!(receipt.is_direct());
    assert!(
        !receipt.fully_accepted_nonempty(),
        "an earlier accepted frame cannot authorize a failed queued key"
    );
    drop(good);
    assert_eq!(cap.drain(), b"first");
}

#[test]
fn failed_pty_write_is_reported_not_falsely_ok() {
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
    let term = term_with(&[]);
    let sink = SinkWriter::new(-1);
    let press = InputEvent::Key {
        key: Key::Named(NamedKey::ArrowUp),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    };
    assert_eq!(
        seam_egress(
            &term,
            &crate::mode_mirror_of(&term),
            &sink,
            &press,
            EgressMode::Interactive
        ),
        Egress::Reported(Delivery::Failed)
    );

    let release = InputEvent::Key {
        key: Key::Named(NamedKey::ArrowUp),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Release,
    };
    assert_eq!(
        seam_egress(
            &term,
            &crate::mode_mirror_of(&term),
            &sink,
            &release,
            EgressMode::Interactive
        ),
        Egress::Reported(Delivery::Full)
    );
}

#[test]
fn key_sequence_egress_is_verbatim_and_reply_faithful() {
    let term = term_with(&[b"\x1b[>1u", b"\x1b[?1h"]);
    assert_eq!(
        egress_bytes(&term, &InputEvent::KeySequence(b"\x1b[15~".to_vec())),
        b"\x1b[15~",
        "mapped bytes are sent verbatim regardless of keyboard mode"
    );
    let empty = InputEvent::KeySequence(Vec::new());
    assert_eq!(egress_bytes(&term, &empty), b"");
    assert_eq!(egress_of(&term, &empty), Egress::Reported(Delivery::Full));
    let sink = SinkWriter::new(-1);
    assert_eq!(
        seam_egress(
            &term,
            &crate::mode_mirror_of(&term),
            &sink,
            &InputEvent::KeySequence(b"hi".to_vec()),
            EgressMode::Interactive
        ),
        Egress::Reported(Delivery::Failed)
    );
}

#[test]
fn ime_commit_report_all_is_one_keyless_event_at_the_pty_seam() {
    let term = term_with(&[b"\x1b[>8u"]);
    assert_eq!(
        egress_bytes(&term, &InputEvent::Text("日本".to_string())),
        b"\x1b[0u"
    );

    let with_text = term_with(&[b"\x1b[>24u"]);
    assert_eq!(
        egress_bytes(&with_text, &InputEvent::Text("日本".to_string())),
        b"\x1b[0;1;26085:26412u"
    );
}

fn term_with(seqs: &[&[u8]]) -> Arc<Mutex<Terminal>> {
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    {
        let mut t = term.lock().unwrap();
        for s in seqs {
            t.process(s);
        }
    }
    term
}

/// A TYPED TURN'S REST REACHES ITS PROGRAM AS TYPING IT WOULD HAVE (the hostile reviews of the
/// turn-deadline work, 2026-11-06): wherever the cap or the pace stops a `turn typed=1`'s
/// keys, the keys typed and the writes of the rest ([`crate::control::rest_writes`], each run
/// built as the turn builds it, [`crate::control::rest_run_event`] under the mirror's keyboard
/// mode) put on the wire the bytes that typing the whole text puts there, byte for byte,
/// whatever keyboard mode the program set: none, DEC 2004 (no bracket is written, so no
/// program's paste handler runs: fish's escaped the quotes of a pasted rest), a kitty flag set
/// and modifyOtherKeys. A trailing literal `\n` stays the two characters it is, and a control
/// character typing drops still parts the graphemes either side of it (`e`, then a combining
/// acute: two keys, never one committed text). Typed and written, the text's keys are counted
/// once each.
/// NEGATIVE CONTROLS: with each run pasted as the turn once pasted it
/// (`control_session::turn_paste_event`),
/// the bytes differ under DEC 2004 (the brackets), under the kitty flags and under
/// modifyOtherKeys (text where the keys are escape codes); with a run's text segmented again
/// for its keys, they differ under kitty's report-all modes for `e\x01\u{301}x`.
#[test]
fn a_typed_turn_s_rest_reaches_its_program_as_typing_it_would() {
    use aterm_grapheme::GraphemeClusters;

    use crate::control::{RestWrite, rest_run_event, rest_writes, typed_key_events};

    let typed = |term: &Mutex<Terminal>, text: &str| -> (Vec<u8>, usize) {
        let keys = typed_key_events(text);
        let bytes = keys
            .iter()
            .flat_map(|(_, ev)| egress_bytes(term, ev))
            .collect();
        (bytes, keys.len())
    };
    let rest = |term: &Mutex<Terminal>, text: &str| -> (Vec<u8>, usize) {
        let keyboard = crate::mode_mirror_of(term).keyboard_mode();
        let (mut bytes, mut keys) = (Vec::new(), 0);
        for write in rest_writes(text) {
            match write {
                RestWrite::Run { keys: run, .. } => {
                    bytes.extend(egress_bytes(term, &rest_run_event(&run, keyboard)));
                    keys += run.len();
                }
                RestWrite::Key(ev) => {
                    bytes.extend(egress_bytes(term, &ev));
                    keys += 1;
                }
            }
        }
        (bytes, keys)
    };
    let modes: [(&str, &[&[u8]]); 6] = [
        ("no mode", &[]),
        ("DEC 2004", &[b"\x1b[?2004h"]),
        ("kitty 1|4 and DEC 2004", &[b"\x1b[?2004h", b"\x1b[>5u"]),
        ("kitty report-all", &[b"\x1b[>8u"]),
        ("kitty report-all with text", &[b"\x1b[>24u"]),
        ("modifyOtherKeys 2", &[b"\x1b[>4;2m"]),
    ];
    for (mode, seqs) in modes {
        let term = term_with(seqs);
        for text in [
            "printf '<%s>\\n' 'fix it'",
            "echo done\\n",
            "col1\tcol2",
            "\t\tlead",
            "trail\t",
            "a\r\nb",
            "x\ry\n",
            "Hi, a-1!",
            "ctl\u{1}\u{1b}x\u{7f}y",
            "e\u{301}\u{1f44d}\u{1f3fd} \u{65e5}\u{672c}",
            "e\u{1}\u{301}x",
            "\u{1f1fa}\u{7f}\u{1f1f8}",
            "a\u{1b}\u{200d}b",
        ] {
            let (whole, keys) = typed(&term, text);
            let cuts = text
                .grapheme_indices()
                .map(|(at, _)| at)
                .chain([text.len()]);
            for cut in cuts {
                let (head, tail) = text.split_at(cut);
                let ((mut split, typed_keys), (rest_bytes, rest_keys)) =
                    (typed(&term, head), rest(&term, tail));
                split.extend(rest_bytes);
                assert_eq!(
                    String::from_utf8_lossy(&split),
                    String::from_utf8_lossy(&whole),
                    "{text:?} cut at {cut}, under {mode}"
                );
                assert_eq!(typed_keys + rest_keys, keys, "{text:?} cut at {cut}");
            }
        }
    }
}

#[test]
fn key_and_text_egress_complete_while_the_terminal_mutex_is_held() {
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
    let term = term_with(&[b"\x1b[>1u"]);
    let modes = crate::mode_mirror_of(&term);
    let mut cap = CaptureSink::new();
    let sink = SinkWriter::new(cap.master());
    let held = term_lock(&term);
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = {
        let term = term.clone();
        std::thread::spawn(move || {
            let press = InputEvent::Key {
                key: Key::Named(NamedKey::Enter),
                mods: Modifiers::SHIFT,
                base_layout: None,
                layout_text: None,
                event_type: KeyEventType::Press,
            };
            let a = seam_egress(&term, &modes, &sink, &press, EgressMode::Interactive);
            let text = InputEvent::Text("é".to_string());
            let b = seam_egress(&term, &modes, &sink, &text, EgressMode::Interactive);
            drop(sink);
            let _ = tx.send((a, b));
        })
    };
    let (a, b) = rx
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("the seam parked behind the terminal mutex to read keyboard_mode()");
    drop(held);
    worker.join().unwrap();
    assert_eq!(a, Egress::Reported(Delivery::Full));
    assert_eq!(b, Egress::Reported(Delivery::Full));
    assert_eq!(cap.drain(), b"\x1b[13;2u\xc3\xa9".to_vec());
}

/// A pointer report the UI thread makes waits as the UI thread: registered, so the PTY reader
/// hands the lock over during a flood, and booked. Each arm of the seam, by both of its routes.
#[test]
fn a_pointer_report_on_the_ui_thread_registers_its_wait_and_books_it() {
    use std::sync::atomic::Ordering;
    let site = TermWaitSite::Mouse;
    let booked = || {
        crate::metrics::METRICS.window.redraw.term_wait[site.index()]
            .copy_out()
            .n()
    };
    let before = booked();
    let pointer = [
        InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
            row: 0,
            col: 0,
            mods: 0,
            px_off: PixelOffset::default(),
            click_count: 1,
            side: SelectionSide::Left,
            block: false,
            suppress_copy_on_select: false,
        },
        InputEvent::MouseMove {
            buttons: 3,
            row: 0,
            col: 0,
            mods: 0,
            px_off: PixelOffset::default(),
            side: SelectionSide::Left,
        },
        wheel(WheelDir::Up, 1),
    ];
    let mut waits = 0;
    for ev in pointer {
        for encode in [false, true] {
            let term = term_with(&[b"\x1b[?1003h\x1b[?1006h"]);
            let modes = crate::mode_mirror_of(&term);
            let waiting = Arc::new(AtomicU32::new(0));
            let held = term_lock(&term);
            let reporter = {
                let (term, waiting, ev) = (term.clone(), waiting.clone(), ev.clone());
                std::thread::spawn(move || {
                    let sink = SinkWriter::new(-1);
                    if encode {
                        let _ = seam_encode_ui(&term, &modes, &sink, &ev, &waiting);
                    } else {
                        let mode = EgressMode::Interactive;
                        let _ = seam_egress_receipt_ui(&term, &modes, &sink, &ev, mode, &waiting);
                    }
                })
            };
            aterm_spec::wait::until_within(
                aterm_spec::wait::MUST_HAPPEN,
                "the report's wait for the held terminal registers",
                || (waiting.load(Ordering::Acquire) == 1).then_some(()),
            );
            drop(held);
            reporter.join().expect("the report finishes");
            assert_eq!(waiting.load(Ordering::Acquire), 0, "{ev:?} let go");
            waits += 1;
        }
    }
    assert!(booked() >= before + waits, "each wait books a mouse wait");
}

fn wheel(dir: WheelDir, lines: i32) -> InputEvent {
    InputEvent::Wheel {
        dir,
        lines,
        detent: true,
        row: 0,
        col: 0,
        mods: 0,
        px_off: PixelOffset::CELL_ORIGIN,
    }
}

#[cfg(unix)]
fn detent_arrows(arrow: &[u8], notches: i32) -> Vec<u8> {
    let lines = wheel_platform_lines(WheelPlatform::Host, notches, 24, true);
    arrow.repeat(usize::try_from(lines).expect("a non-negative count"))
}

fn egress_of(term: &Mutex<Terminal>, ev: &InputEvent) -> Egress {
    let cap = CaptureSink::new();
    let sink = SinkWriter::new(cap.master());
    seam_egress(
        term,
        &crate::mode_mirror_of(term),
        &sink,
        ev,
        EgressMode::Interactive,
    )
}

#[test]
fn the_option_wheel_override_routes_locally_on_every_platform() {
    use aterm_types::mouse::{ALT_MASK, SHIFT_MASK};

    let route = |term: &Mutex<Terminal>, mods: u8| {
        let t = term.lock().expect("terminal");
        super::wheel_route_for(&t, mods)
    };

    let tracking = term_with(&[b"\x1b[?1000h", b"\x1b[?1006h"]);
    assert_eq!(
        route(&tracking, 0),
        WheelRoute::Report,
        "control: a tracking app still owns the plain wheel"
    );
    assert_eq!(
        route(&tracking, ALT_MASK),
        WheelRoute::Viewport,
        "Option+wheel must reach the local scrollback under tracking"
    );
    assert_eq!(
        route(&tracking, SHIFT_MASK),
        WheelRoute::Viewport,
        "…and the older Shift bypass is untouched"
    );

    let alt_tracking = term_with(&[b"\x1b[?1049h", b"\x1b[?1000h", b"\x1b[?1006h"]);
    assert_eq!(
        route(&alt_tracking, ALT_MASK),
        WheelRoute::Report,
        "a mouse-owning alt-screen app keeps the wheel even with Option held"
    );
    let alt_scroll = term_with(&[b"\x1b[?1007h", b"\x1b[?1049h"]);
    assert_eq!(
        route(&alt_scroll, ALT_MASK),
        WheelRoute::AltScroll,
        "alternate scroll still converts the wheel to an arrow"
    );

    let plain = term_with(&[]);
    assert_eq!(route(&plain, 0), WheelRoute::Viewport);
    assert_eq!(route(&plain, ALT_MASK), WheelRoute::Viewport);

    let main_1007 = term_with(&[b"\x1b[?1007h"]);
    assert_eq!(route(&main_1007, 0), WheelRoute::Viewport);
    let alt_1007_off = term_with(&[b"\x1b[?1049h", b"\x1b[?1007l"]);
    assert_eq!(route(&alt_1007_off, 0), WheelRoute::Viewport);
}

#[test]
fn the_mirrored_wheel_route_equals_the_locked_one_for_every_mode_combination() {
    use aterm_types::mouse::{ALT_MASK, SHIFT_MASK};

    let mut routes = std::collections::BTreeSet::new();
    for tracking in [false, true] {
        for alt_screen in [false, true] {
            for alt_scroll in [false, true] {
                let mut seqs: Vec<&[u8]> = vec![if alt_scroll {
                    b"\x1b[?1007h"
                } else {
                    b"\x1b[?1007l"
                }];
                if alt_screen {
                    seqs.push(b"\x1b[?1049h");
                }
                if tracking {
                    seqs.push(b"\x1b[?1000h\x1b[?1006h");
                }
                let term = term_with(&seqs);
                let t = term.lock().expect("terminal");
                for mods in [0, SHIFT_MASK, ALT_MASK, SHIFT_MASK | ALT_MASK] {
                    let locked = super::wheel_route_for(&t, mods);
                    assert_eq!(
                        super::wheel_route_mirrored(t.mode_mirror(), mods),
                        locked,
                        "tracking={tracking} alt_screen={alt_screen} \
                         alt_scroll={alt_scroll} mods={mods:#x}"
                    );
                    routes.insert(format!("{locked:?}"));
                }
            }
        }
    }
    assert_eq!(
        routes.len(),
        3,
        "non-vacuous: the sweep reached every route ({routes:?})"
    );
}

#[cfg(unix)]
#[test]
fn alt_screen_alternate_scroll_converts_wheel_to_arrows() {
    let term = term_with(&[b"\x1b[?1007h", b"\x1b[?1049h"]);
    assert_eq!(
        egress_bytes(&term, &wheel(WheelDir::Up, 2)),
        detent_arrows(b"\x1b[A", 2)
    );
    assert_eq!(
        egress_bytes(&term, &wheel(WheelDir::Down, 1)),
        detent_arrows(b"\x1b[B", 1)
    );
    term.lock().unwrap().process(b"\x1b[?1h");
    assert_eq!(
        egress_bytes(&term, &wheel(WheelDir::Up, 1)),
        detent_arrows(b"\x1bOA", 1)
    );
    assert!(matches!(
        egress_of(&term, &wheel(WheelDir::Up, 1)),
        Egress::Reported(_)
    ));
}

#[test]
fn shift_wheel_bypasses_a_tracking_app_with_no_report_bytes() {
    let term = term_with(&[
        b"\x1b[?1049h",
        b"\x1b[?1007h",
        b"\x1b[?1000h",
        b"\x1b[?1006h",
    ]);
    let shifted = InputEvent::Wheel {
        dir: WheelDir::Up,
        lines: 2,
        detent: true,
        row: 0,
        col: 0,
        mods: aterm_types::mouse::SHIFT_MASK,
        px_off: PixelOffset::CELL_ORIGIN,
    };
    assert!(matches!(
        egress_of(&term, &shifted),
        Egress::TrackingOff {
            wheel_lines: 2,
            wheel_up: true,
            ..
        }
    ));
    assert!(egress_bytes(&term, &shifted).is_empty());
    assert_eq!(
        egress_bytes(&term, &wheel(WheelDir::Up, 1)),
        b"\x1b[<64;1;1M"
    );
}

#[test]
fn a_viewport_wheel_carries_the_engine_facts_from_its_one_lock() {
    let term = term_with(&[]);
    {
        let mut t = term.lock().unwrap();
        for i in 0..40 {
            t.process(format!("line {i}\r\n").as_bytes());
        }
        t.scroll_display(3);
    }
    let expected = {
        let t = term.lock().unwrap();
        WheelViewport {
            display_offset: t.grid().display_offset(),
            scrollback_lines: t.grid().scrollback_lines(),
            rows: t.rows(),
        }
    };
    assert!(expected.display_offset == 3 && expected.scrollback_lines >= 16);
    assert_eq!(
        egress_of(&term, &wheel(WheelDir::Up, 2)),
        Egress::TrackingOff {
            wheel_lines: 2,
            wheel_up: true,
            viewport: Some(expected),
        }
    );
    let release = InputEvent::MouseButton {
        button: aterm_types::mouse::MouseButton::Left,
        pressed: false,
        row: 0,
        col: 0,
        mods: 0,
        click_count: 1,
        side: aterm_core::selection::SelectionSide::Left,
        block: false,
        suppress_copy_on_select: false,
        px_off: PixelOffset::CELL_ORIGIN,
    };
    assert!(matches!(
        egress_of(&term, &release),
        Egress::TrackingOff { viewport: None, .. }
    ));
}

#[test]
fn horizontal_wheel_reports_only_while_tracking() {
    let sink = SinkWriter::new(-1);
    for dir in [WheelDir::Left, WheelDir::Right] {
        let tracking = term_with(&[b"\x1b[?1000h", b"\x1b[?1006h"]);
        assert!(
            matches!(
                seam_egress(
                    &tracking,
                    &crate::mode_mirror_of(&tracking),
                    &sink,
                    &wheel(dir, 2),
                    EgressMode::Interactive
                ),
                Egress::Reported(_)
            ),
            "{dir:?} must reach a tracking app"
        );
        let idle = term_with(&[]);
        assert!(
            matches!(
                seam_egress(
                    &idle,
                    &crate::mode_mirror_of(&idle),
                    &sink,
                    &wheel(dir, 2),
                    EgressMode::Interactive
                ),
                Egress::TrackingOff {
                    wheel_lines: 0,
                    wheel_up: false,
                    ..
                }
            ),
            "{dir:?} must move nothing locally"
        );
        let pager = term_with(&[b"\x1b[?1049h", b"\x1b[?1007h"]);
        assert!(
            matches!(
                seam_egress(
                    &pager,
                    &crate::mode_mirror_of(&pager),
                    &sink,
                    &wheel(dir, 2),
                    EgressMode::Interactive
                ),
                Egress::TrackingOff {
                    wheel_lines: 0,
                    wheel_up: false,
                    ..
                }
            ),
            "{dir:?} must not synthesize arrows for a pager"
        );
        let shifted = InputEvent::Wheel {
            dir,
            lines: 2,
            detent: true,
            row: 0,
            col: 0,
            mods: aterm_types::mouse::SHIFT_MASK,
            px_off: PixelOffset::CELL_ORIGIN,
        };
        let tracking = term_with(&[b"\x1b[?1000h", b"\x1b[?1006h"]);
        assert!(
            matches!(
                seam_egress(
                    &tracking,
                    &crate::mode_mirror_of(&tracking),
                    &sink,
                    &shifted,
                    EgressMode::Interactive
                ),
                Egress::TrackingOff {
                    wheel_lines: 0,
                    wheel_up: false,
                    ..
                }
            ),
            "shift+{dir:?} bypasses to a viewport that cannot pan"
        );
    }
    let idle = term_with(&[]);
    assert!(matches!(
        seam_egress(
            &idle,
            &crate::mode_mirror_of(&idle),
            &sink,
            &wheel(WheelDir::Up, 2),
            EgressMode::Interactive
        ),
        Egress::TrackingOff {
            wheel_lines: 2,
            wheel_up: true,
            ..
        }
    ));
}

#[test]
fn the_report_burst_is_clamped_at_both_ends_for_every_source() {
    let term = term_with(&[b"\x1b[?1000h", b"\x1b[?1006h"]);
    let one = b"\x1b[<64;1;1M";
    assert_eq!(
        egress_bytes(&term, &wheel(WheelDir::Up, 3)).len(),
        one.len() * 3
    );
    assert_eq!(
        egress_bytes(&term, &wheel(WheelDir::Up, MAX_WHEEL_BURST)).len(),
        one.len() * MAX_WHEEL_BURST as usize
    );
    assert_eq!(
        egress_bytes(&term, &wheel(WheelDir::Up, MAX_WHEEL_BURST + 3)).len(),
        one.len() * MAX_WHEEL_BURST as usize,
        "one event cannot flood the PTY past the ceiling"
    );
    assert_eq!(
        egress_bytes(&term, &wheel(WheelDir::Left, MAX_WHEEL_BURST + 3)).len(),
        one.len() * MAX_WHEEL_BURST as usize,
        "the horizontal axis takes the same ceiling"
    );
    assert_eq!(egress_bytes(&term, &wheel(WheelDir::Up, 0)), one);
    assert_eq!(egress_bytes(&term, &wheel(WheelDir::Up, -7)), one);
}

#[test]
fn wheel_scale_is_the_platform_distance_not_one_line() {
    let windows = |notch| WheelPlatform::Windows(notch);
    assert_eq!(
        wheel_platform_lines(windows(WheelNotch::Lines(3)), 1, 24, true),
        3
    );
    assert_eq!(
        wheel_platform_lines(windows(WheelNotch::Page), 2, 50, true),
        100
    );
    assert_eq!(
        wheel_platform_lines(windows(WheelNotch::Lines(0)), 9, 24, true),
        0
    );
    assert_eq!(wheel_scaled_lines(1, WheelNotch::Lines(3), 24), 3);
    assert_eq!(wheel_scaled_lines(4, WheelNotch::Lines(3), 24), 12);
    assert_eq!(wheel_scaled_lines(2, WheelNotch::Lines(1), 24), 2);
    assert_eq!(wheel_scaled_lines(1, WheelNotch::Page, 24), 24);
    assert_eq!(wheel_scaled_lines(2, WheelNotch::Page, 50), 100);
    assert_eq!(wheel_scaled_lines(1, WheelNotch::Page, 0), 1);
    assert_eq!(wheel_scaled_lines(9, WheelNotch::Lines(0), 24), 0);
    assert_eq!(
        wheel_scaled_lines(i32::MAX, WheelNotch::Lines(u32::MAX), 24),
        i32::MAX
    );
}

#[test]
fn a_free_desktop_wheel_detent_is_vtes_distance() {
    let vte = WheelPlatform::Vte;
    assert_eq!(vte_lines_per_detent(24), 3);
    assert_eq!(wheel_platform_lines(vte, 1, 24, true), 3);
    assert_eq!(wheel_platform_lines(vte, 4, 24, true), 12);
    assert_eq!(vte_lines_per_detent(20), 2);
    assert_eq!(vte_lines_per_detent(21), 3);
    assert_eq!(vte_lines_per_detent(50), 5);
    assert_eq!(vte_lines_per_detent(10), 1);
    assert_eq!(vte_lines_per_detent(1), 1);
    assert_eq!(vte_lines_per_detent(0), 1);
    assert_eq!(vte_lines_per_detent(u16::MAX), 6554);
    assert_eq!(wheel_platform_lines(vte, i32::MAX, 24, true), i32::MAX);
    assert_eq!(wheel_platform_lines(WheelPlatform::Mac, 4, 24, true), 4);
}

#[test]
fn whether_alternate_scroll_sends_an_arrow_needs_no_page_height() {
    for platform in WheelPlatform::ARMS.into_iter().chain([WheelPlatform::Host]) {
        for rows in [0, 1, 10, 11, 24, 50, u16::MAX] {
            for lines in [1, 2, 7, MAX_WHEEL_BURST] {
                for detent in [true, false] {
                    assert_eq!(
                        alt_scroll_repeat(platform, lines, rows, detent) > 0,
                        alt_scroll_sends(platform, lines, detent),
                        "{lines} line(s) at {rows} rows, detent={detent}, {platform:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn a_wheel_reaches_the_program_exactly_where_the_seam_sends_it_bytes() {
    use aterm_types::mouse::{ALT_MASK, SHIFT_MASK};
    let modes: [&[&[u8]]; 10] = [
        &[],
        &[b"\x1b[?9h"],
        &[b"\x1b[?1000h"],
        &[b"\x1b[?1002h", b"\x1b[?1006h"],
        &[b"\x1b[?1003h"],
        &[b"\x1b[?1049h"],
        &[b"\x1b[?1049h", b"\x1b[?1007h"],
        &[b"\x1b[?1049h", b"\x1b[?1007h", b"\x1b[?1h"],
        &[b"\x1b[?1049h", b"\x1b[?1007h", b"\x1b[?9h"],
        &[b"\x1b[?1049h", b"\x1b[?1007h", b"\x1b[?1000h"],
    ];
    let x10 = String::from_utf8_lossy(b"\x1b[?9h").into_owned();
    let alt_scroll = String::from_utf8_lossy(b"\x1b[?1049h\x1b[?1007h").into_owned();
    let mut swallowed = Vec::new();
    for set in modes {
        let name = String::from_utf8_lossy(&set.concat()).into_owned();
        for dir in [
            WheelDir::Up,
            WheelDir::Down,
            WheelDir::Left,
            WheelDir::Right,
        ] {
            for mods in [0, SHIFT_MASK, ALT_MASK] {
                for detent in [true, false] {
                    let term = term_with(set);
                    let ev = InputEvent::Wheel {
                        dir,
                        lines: 1,
                        detent,
                        row: 0,
                        col: 0,
                        mods,
                        px_off: PixelOffset::CELL_ORIGIN,
                    };
                    let route = wheel_route_for(&term.lock().unwrap(), mods);
                    let reaches = crate::human_input::reaches_program(
                        &ev,
                        &crate::mode_mirror_of(&term),
                        WheelPlatform::Host,
                    );
                    let sent = !egress_bytes(&term, &ev).is_empty();
                    assert_eq!(
                        reaches, sent,
                        "{name:?} {dir:?} mods={mods} detent={detent} on {route:?}"
                    );
                    if route != WheelRoute::Viewport && !sent && mods == 0 {
                        swallowed.push((name.clone(), dir, route));
                    }
                }
            }
        }
    }
    for witness in [
        (x10, WheelDir::Up, WheelRoute::Report),
        (alt_scroll, WheelDir::Left, WheelRoute::AltScroll),
    ] {
        assert!(
            swallowed.contains(&witness),
            "{witness:?} is not swallowed: {swallowed:?}"
        );
    }
}

#[test]
fn a_precise_wheel_row_is_never_multiplied() {
    for platform in WheelPlatform::ARMS.into_iter().chain([WheelPlatform::Host]) {
        for rows in [0, 1, 24, 50, u16::MAX] {
            for lines in [0, 1, 2, 7, MAX_WHEEL_BURST] {
                assert_eq!(wheel_platform_lines(platform, lines, rows, false), lines);
            }
        }
    }
}

#[test]
fn a_free_desktop_detent_is_three_pager_arrows_but_one_tracking_report() {
    let vte = |term: &Mutex<Terminal>, ev: &InputEvent| {
        let sink = SinkWriter::new(-1);
        seam_encode_on(
            WheelPlatform::Vte,
            term,
            &crate::mode_mirror_of(term),
            &sink,
            ev,
        )
        .1
    };
    let precise = |dir| InputEvent::Wheel {
        dir,
        lines: 1,
        detent: false,
        row: 0,
        col: 0,
        mods: 0,
        px_off: PixelOffset::CELL_ORIGIN,
    };
    let pager = term_with(&[b"\x1b[?1007h", b"\x1b[?1049h"]);
    assert_eq!(
        vte(&pager, &wheel(WheelDir::Down, 1)),
        b"\x1b[B\x1b[B\x1b[B"
    );
    assert_eq!(vte(&pager, &wheel(WheelDir::Up, 2)), b"\x1b[A".repeat(6));
    assert_eq!(vte(&pager, &precise(WheelDir::Down)), b"\x1b[B");
    let tracking = term_with(&[b"\x1b[?1000h", b"\x1b[?1006h"]);
    assert_eq!(
        egress_bytes(&tracking, &wheel(WheelDir::Down, 1)),
        b"\x1b[<65;1;1M"
    );
    assert_eq!(
        egress_bytes(&tracking, &wheel(WheelDir::Up, 2)),
        b"\x1b[<64;1;1M".repeat(2)
    );
    assert!(matches!(
        egress_of(&term_with(&[]), &wheel(WheelDir::Up, 1)),
        Egress::TrackingOff {
            wheel_lines: 1,
            wheel_up: true,
            ..
        }
    ));
}

#[test]
fn builders_converge() {
    use aterm_types::keyboard::{Key, Modifiers, NamedKey};
    use aterm_window::keyboard::{Key as WinitKey, KeyCode, NamedKey as WinitNamed, PhysicalKey};
    use aterm_winit_keymap::{base_layout_key_for, map_logical_key};

    let human_up = {
        let key = map_logical_key(&WinitKey::Named(WinitNamed::ArrowUp)).expect("up maps");
        let base = base_layout_key_for(PhysicalKey::Code(KeyCode::ArrowUp));
        InputEvent::Key {
            key,
            mods: Modifiers::empty(),
            base_layout: base,
            layout_text: None,
            event_type: aterm_types::keyboard::KeyEventType::Press,
        }
    };
    let ctrl_up = crate::control::parse_key("up").expect("key up parses");
    assert_eq!(
        human_up, ctrl_up,
        "human `Up` builder != controller `key up` builder"
    );
    assert_eq!(
        human_up,
        InputEvent::Key {
            key: Key::Named(NamedKey::ArrowUp),
            mods: Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: aterm_types::keyboard::KeyEventType::Press
        },
    );

    let human_ctrl_c = {
        let key = map_logical_key(&WinitKey::Character("c".into())).expect("c maps");
        let base = base_layout_key_for(PhysicalKey::Code(KeyCode::KeyC));
        InputEvent::Key {
            key,
            mods: Modifiers::CTRL,
            base_layout: base,
            layout_text: None,
            event_type: aterm_types::keyboard::KeyEventType::Press,
        }
    };
    let ctrl_ctrl_c = crate::control::parse_ctrl("c").expect("ctrl c parses");
    for seqs in [&[][..], &[&b"\x1b[>1u"[..]][..]] {
        let term = term_with(seqs);
        assert_eq!(
            egress_bytes(&term, &human_ctrl_c),
            egress_bytes(&term, &ctrl_ctrl_c),
            "Ctrl+C bytes diverge (human base_layout vs controller None) under {seqs:?}",
        );
    }

    let ctrl_press =
        crate::control::parse_mouse("press left 5 9 mods=shift count=2 side=right block=1")
            .expect("mouse press parses");
    let human_press = InputEvent::MouseButton {
        button: aterm_types::mouse::MouseButton::Left,
        pressed: true,
        row: 5,
        col: 9,
        mods: aterm_types::mouse::SHIFT_MASK,
        click_count: 2,
        side: SelectionSide::Right,
        block: true,
        suppress_copy_on_select: false,
        px_off: PixelOffset::CELL_ORIGIN,
    };
    assert_eq!(human_press, ctrl_press, "mouse-press builder mismatch");

    let ctrl_wheel = crate::control::parse_mouse("wheelup left 2 4").expect("wheelup parses");
    let human_wheel = InputEvent::Wheel {
        dir: WheelDir::Up,
        lines: 1,
        detent: true,
        row: 2,
        col: 4,
        mods: 0,
        px_off: PixelOffset::CELL_ORIGIN,
    };
    assert_eq!(human_wheel, ctrl_wheel, "wheel builder mismatch");
}

#[test]
fn sgr_pixel_1016_reports_true_pixel_coordinate() {
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    {
        let mut t = term.lock().unwrap();
        t.set_cell_pixel_size(10, 20);
        t.process(b"\x1b[?1000h");
        t.process(b"\x1b[?1016h");
    }
    let press = InputEvent::MouseButton {
        button: MouseButton::Left,
        pressed: true,
        row: 2,
        col: 3,
        mods: 0,
        click_count: 1,
        side: SelectionSide::Left,
        block: false,
        suppress_copy_on_select: false,
        px_off: PixelOffset { x: 4, y: 7 },
    };
    assert_eq!(egress_bytes(&term, &press), b"\x1b[<0;35;48M");

    let term_cell = term_with(&[b"\x1b[?1000h", b"\x1b[?1006h"]);
    assert_eq!(egress_bytes(&term_cell, &press), b"\x1b[<0;4;3M");
}

#[test]
fn jump_prompt_target_finds_prompts_from_an_a_b_only_shell() {
    let mut t = Terminal::new(4, 20);
    t.process(b"\x1b]133;A\x1b\\p1>\x1b]133;B\x1b\\\r\n");
    for _ in 0..6 {
        t.process(b"out\r\n");
    }
    t.process(b"\x1b]133;A\x1b\\p2>\x1b]133;B\x1b\\\r\n");
    for _ in 0..6 {
        t.process(b"out\r\n");
    }
    assert!(
        t.command_marks().is_empty(),
        "an A/B-only shell records no completed-command marks — that is the \
         whole point of the union"
    );
    let prompts: Vec<u64> = t.all_blocks().map(|b| b.prompt_start_row).collect();
    assert_eq!(prompts.len(), 2, "two prompts were marked: {prompts:?}");

    t.scroll_to_bottom();
    let top = t.grid().top_visible_absolute_row();
    let older: Vec<u64> = prompts.iter().copied().filter(|&r| r < top).collect();
    assert!(
        !older.is_empty(),
        "the fixture must leave a prompt above the viewport: top={top} \
         prompts={prompts:?}"
    );
    assert_eq!(
        jump_prompt_target(&t, true),
        older.iter().copied().max(),
        "prev-prompt must reach the nearest prompt above the viewport"
    );

    t.scroll_to_top();
    let top = t.grid().top_visible_absolute_row();
    assert_eq!(
        jump_prompt_target(&t, false),
        prompts.iter().copied().filter(|&r| r > top).min(),
        "next-prompt must reach the nearest prompt below the viewport"
    );
}

#[test]
fn jump_prompt_target_is_unchanged_for_a_fully_marked_shell() {
    let mut t = Terminal::new(4, 20);
    for n in 0..3 {
        t.process(b"\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\cmd\r\n\x1b]133;C\x1b\\");
        for _ in 0..5 {
            t.process(b"out\r\n");
        }
        let _ = n;
        t.process(b"\x1b]133;D;0\x1b\\");
    }
    let marks: Vec<u64> = t
        .command_marks()
        .iter()
        .map(|m| m.prompt_start_row)
        .collect();
    assert_eq!(marks.len(), 3, "three completed commands: {marks:?}");
    t.scroll_to_bottom();
    let top = t.grid().top_visible_absolute_row();
    assert_eq!(
        jump_prompt_target(&t, true),
        marks.iter().copied().filter(|&r| r < top).max()
    );
    assert_eq!(
        jump_prompt_target(&t, false),
        marks.iter().copied().filter(|&r| r > top).min()
    );
}

#[test]
fn report_coords_selects_pixel_only_for_1016() {
    let mut t = Terminal::new(24, 80);
    t.set_cell_pixel_size(10, 20);
    t.process(b"\x1b[?1000h");
    t.process(b"\x1b[?1006h");
    assert_eq!(report_coords(&t, 3, 2, PixelOffset { x: 9, y: 19 }), (3, 2));
    t.process(b"\x1b[?1016h");
    assert_eq!(
        report_coords(&t, 3, 2, PixelOffset { x: 4, y: 7 }),
        (34, 47)
    );
    assert_eq!(
        report_coords(&t, 3, 2, PixelOffset { x: 99, y: 99 }),
        (3 * 10 + 9, 2 * 20 + 19)
    );
}

#[test]
fn shell_escape_path_leaves_plain_paths_unchanged() {
    assert_eq!(
        shell_escape_path_posix("/home/user/Downloads/report-2026_v2.final.pdf"),
        "/home/user/Downloads/report-2026_v2.final.pdf"
    );
    assert_eq!(shell_escape_path_posix("/tmp/a:b,c%20d"), "/tmp/a:b,c%20d");
    assert_eq!(
        shell_escape_path_posix("/Users//me/Café/Ünïcødé.txt"),
        "/Users//me/Café/Ünïcødé.txt"
    );
    assert_eq!(shell_escape_path_posix(""), "");
}

#[test]
fn shell_escape_path_matches_iterm_set_exactly() {
    let set = "\\ ()\"&'!$<>;|*?[]#`\t{}^+=@~\r\n";
    let escaped = shell_escape_path_posix(set);
    let expected: String = set.chars().flat_map(|c| ['\\', c]).collect();
    assert_eq!(escaped, expected);
    let inert = "/.-_,:%0123456789abzABZ";
    assert_eq!(shell_escape_path_posix(inert), inert);
}

#[test]
fn shell_escape_path_neutralises_injection() {
    let attack = "/tmp/$(touch pwned);rm -rf ~ `id` | tee >out & evil!.txt";
    let escaped = shell_escape_path_posix(attack);
    let bytes: Vec<char> = escaped.chars().collect();
    for (i, &c) in bytes.iter().enumerate() {
        if matches!(
            c,
            '(' | ')' | '$' | ';' | '|' | '&' | '`' | '>' | '<' | '!' | ' ' | '~'
        ) {
            assert!(
                i > 0 && bytes[i - 1] == '\\',
                "unescaped {c:?} at {i} in {escaped:?}"
            );
        }
    }
}

#[test]
fn the_seam_frames_a_paste_by_the_event_not_by_the_moment() {
    let live_off = term_with(&[]);
    let live_on = term_with(&[b"\x1b[?2004h"]);
    let paste = |framing| InputEvent::Paste("ls\nrm -rf ~".to_string(), framing);

    assert_eq!(
        egress_bytes(&live_off, &paste(PasteFraming::AtDrain)),
        b"ls\rrm -rf ~".to_vec()
    );
    assert_eq!(
        egress_bytes(&live_on, &paste(PasteFraming::AtDrain)),
        b"\x1b[200~ls\rrm -rf ~\x1b[201~".to_vec()
    );

    assert_eq!(
        egress_bytes(&live_on, &paste(PasteFraming::Gesture { bracketed: false })),
        b"ls\rrm -rf ~".to_vec(),
        "2004 turned ON after the gesture cannot bracket an unbracketed promise"
    );
    assert_eq!(
        egress_bytes(&live_off, &paste(PasteFraming::Gesture { bracketed: true })),
        b"\x1b[200~ls\rrm -rf ~\x1b[201~".to_vec(),
        "2004 turned OFF after the gesture cannot unframe a paste judged inert"
    );
}

#[cfg(unix)]
#[test]
fn dropped_path_pastes_escaped_with_trailing_space() {
    let drop_text = |p: &str| {
        let mut t = shell_escape_path(p);
        t.push(' ');
        t
    };
    let term = term_with(&[]);
    assert_eq!(
        egress_bytes(
            &term,
            &InputEvent::Paste(drop_text("/p/My File"), PasteFraming::AtDrain)
        ),
        b"/p/My\\ File ".to_vec()
    );
    let term = term_with(&[b"\x1b[?2004h"]);
    assert_eq!(
        egress_bytes(
            &term,
            &InputEvent::Paste(drop_text("/p/My File"), PasteFraming::AtDrain)
        ),
        b"\x1b[200~/p/My\\ File \x1b[201~".to_vec()
    );
}

#[test]
fn shell_escape_path_windows_leaves_plain_paths_unchanged() {
    assert_eq!(
        shell_escape_path_windows("C:\\Users\\me\\report-2026_v2.final.pdf"),
        "C:\\Users\\me\\report-2026_v2.final.pdf"
    );
    assert_eq!(
        shell_escape_path_windows("C:\\Users\\me\\Café\\Ünïcødé.txt"),
        "C:\\Users\\me\\Café\\Ünïcødé.txt"
    );
    assert_eq!(shell_escape_path_windows(""), "");
}

#[test]
fn shell_escape_path_windows_quotes_spaces_keeping_separators() {
    assert_eq!(
        shell_escape_path_windows("C:\\Program Files\\a.txt"),
        "\"C:\\Program Files\\a.txt\""
    );
    assert_eq!(
        shell_escape_path_windows("C:\\Users\\me\\My File (1).png"),
        "\"C:\\Users\\me\\My File (1).png\""
    );
}

#[test]
fn shell_escape_path_windows_quotes_metachars_and_doubles_quotes() {
    assert_eq!(shell_escape_path_windows("C:\\a,b.txt"), "\"C:\\a,b.txt\"");
    assert_eq!(
        shell_escape_path_windows("C:\\a&b;c|d.txt"),
        "\"C:\\a&b;c|d.txt\""
    );
    assert_eq!(shell_escape_path_windows("a\"b"), "\"a\"\"b\"");
}

#[test]
fn shell_escape_path_windows_never_leaves_an_expansion_live() {
    let dl = "C:\\Users\\me\\Downloads\\";
    for (name, want) in [
        ("a$(calc).pdf", "'a$(calc).pdf'"),
        ("a${env:PATH}.pdf", "'a${env:PATH}.pdf'"),
        ("a$x b.pdf", "'a$x b.pdf'"),
        ("a`$(calc)`.pdf", "'a`$(calc)`.pdf'"),
        (
            "a\u{201D};calc;\u{201D}.pdf",
            "'a\u{201D};calc;\u{201D}.pdf'",
        ),
        ("a\u{201C} b.pdf", "'a\u{201C} b.pdf'"),
        ("a\u{201E} b.pdf", "'a\u{201E} b.pdf'"),
        ("O'Neil $(x).pdf", "'O''Neil $(x).pdf'"),
        (
            "a\u{2019};calc;\u{2018}$(x).pdf",
            "'a\u{2019}\u{2019};calc;\u{2018}\u{2018}$(x).pdf'",
        ),
        ("a&calc&$(x).pdf", "'a^&calc^&$(x).pdf'"),
        ("a^&$(x).pdf", "'a^^^&$(x).pdf'"),
    ] {
        assert_eq!(
            shell_escape_path_windows(&format!("{dl}{name}")),
            want.replacen('\'', &format!("'{dl}"), 1),
            "{name:?}"
        );
    }
    assert_eq!(
        shell_escape_path_windows("\\\\host\\C$\\My Docs\\a.txt"),
        "\"\\\\host\\C$\\My Docs\\a.txt\""
    );
    assert_eq!(shell_escape_path_windows("C:\\x y$"), "\"C:\\x y$\"");
}

#[test]
fn paths_paste_insertion_matches_the_drop_contract() {
    let one = vec!["C:\\Users\\me\\a.txt".to_string()];
    assert_eq!(
        paths_paste_insertion(&one),
        format!("{} ", shell_escape_path("C:\\Users\\me\\a.txt"))
    );
    let many = vec![
        "C:\\Program Files\\a.txt".to_string(),
        "C:\\b (1).png".to_string(),
    ];
    let expect = format!(
        "{} {} ",
        shell_escape_path("C:\\Program Files\\a.txt"),
        shell_escape_path("C:\\b (1).png")
    );
    assert_eq!(paths_paste_insertion(&many), expect);
    assert!(expect.ends_with(' ') && !expect.contains('\n'));
    assert_eq!(paths_paste_insertion(&[]), "");
    assert_eq!(paths_paste_insertion(&[String::new()]), "");
}

#[test]
fn wheel_route_shift_bypasses_tracking_before_every_other_test() {
    use super::{WheelRoute, wheel_route};
    for tracking in [false, true] {
        for alt_scroll in [false, true] {
            assert_eq!(
                wheel_route(true, false, tracking, alt_scroll),
                WheelRoute::Viewport,
                "shift must bypass (tracking={tracking}, alt_scroll={alt_scroll})"
            );
        }
    }
    assert_eq!(wheel_route(false, false, true, false), WheelRoute::Report);
    assert_eq!(wheel_route(false, false, true, true), WheelRoute::Report);
    assert_eq!(
        wheel_route(false, false, false, true),
        WheelRoute::AltScroll
    );
    assert_eq!(
        wheel_route(false, false, false, false),
        WheelRoute::Viewport
    );
    assert_eq!(wheel_route(false, true, true, false), WheelRoute::Viewport);
    assert_eq!(wheel_route(false, true, false, false), WheelRoute::Viewport);
    assert_eq!(wheel_route(false, true, true, true), WheelRoute::Viewport);
    assert_eq!(wheel_route(true, true, true, true), WheelRoute::Viewport);
}
