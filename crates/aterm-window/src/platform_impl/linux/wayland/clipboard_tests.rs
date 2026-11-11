// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

use super::*;

/// One seat, the compositor behind it, and the clipboard a SEPARATE client
/// reads — the measured rules and nothing else.
struct FakeSeat {
    /// The shared own-selection slot: what a source of OURS serves when a paste
    /// asks for it.
    slot: String,
    /// The source we offer, if we offer one.
    offered: Option<u32>,
    /// How many sources have been minted; the next one's id.
    minted: u32,
    /// Whether the seat's keyboard is on one of our surfaces.
    focused: bool,
    /// Whether the compositor binds a device for this selection at all.
    device: bool,
    /// The selection: which of OUR sources holds it, and the serial behind it.
    selection: Option<(u32, u32)>,
    /// What the compositor answers with once no source of ours holds the
    /// selection.
    cached: String,
    /// Every protocol step this client sent, in order.
    wire: Vec<Step>,
}

/// The two requests a copy can send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// `wl_data_source.destroy` — aterm-wayland's `Source` sends it from its `Drop`.
    Destroy(u32),
    /// `wl_data_device.set_selection(source, serial)`.
    SetSelection { source: u32, serial: u32 },
}

impl FakeSeat {
    /// A focused client, on a compositor with a device, owning nothing yet.
    fn new() -> Self {
        Self {
            slot: String::new(),
            offered: None,
            minted: 0,
            focused: true,
            device: true,
            selection: None,
            cached: String::new(),
            wire: Vec::new(),
        }
    }

    /// One copy, the way the real one arrives: the handle writes the shared slot
    /// on the calling thread, then the loop runs the port.
    fn copy_text(&mut self, text: &str, serial: u32) -> CopyOutcome {
        self.slot = text.to_owned();
        self.copy(WaylandSelection::Clipboard, serial)
    }

    /// What another client on the system reads.
    fn read(&self) -> String {
        match self.selection {
            Some(_) => self.slot.clone(),
            None => self.cached.clone(),
        }
    }

    /// The proxy is destroyed.
    fn destroy(&mut self, source: u32) {
        self.wire.push(Step::Destroy(source));
        if self.selection.is_some_and(|(held, _)| held == source) {
            self.selection = None;
        }
    }
}

impl SelectionPort for FakeSeat {
    fn focused(&self) -> bool {
        self.focused
    }

    fn offers(&self, _selection: WaylandSelection) -> bool {
        self.offered.is_some()
    }

    fn release(&mut self, _selection: WaylandSelection) {
        if let Some(source) = self.offered.take() {
            self.destroy(source);
        }
    }

    fn claim(&mut self, _selection: WaylandSelection, serial: u32) -> bool {
        if !self.device {
            return false;
        }
        self.minted += 1;
        let source = self.minted;
        self.wire.push(Step::SetSelection { source, serial });
        let dropped = !self.focused || self.selection.is_some_and(|(_, behind)| serial <= behind);
        if !dropped {
            self.selection = Some((source, serial));
            self.cached = self.slot.clone();
        }
        if let Some(displaced) = self.offered.replace(source) {
            self.destroy(displaced);
        }
        true
    }
}

const FOUR: [&str; 4] = ["ZULU-00-aaaa", "ZULU-01-bbbb", "ZULU-02-cccc", "ZULU-03-dddd"];

#[test]
fn a_copy_with_no_fresh_input_behind_it_still_reaches_the_system_clipboard() {
    let mut unreleasing = FakeSeat::new();
    let lost: Vec<String> = FOUR
        .iter()
        .map(|text| {
            unreleasing.slot = (*text).to_owned();
            unreleasing.claim(WaylandSelection::Clipboard, 4426);
            unreleasing.read()
        })
        .collect();
    assert_eq!(
        lost,
        ["ZULU-00-aaaa", "ZULU-00-aaaa", "ZULU-02-cccc", "ZULU-02-cccc"],
        "negative control: claiming without releasing loses every other copy",
    );

    let mut seat = FakeSeat::new();
    for text in FOUR {
        assert!(seat.copy_text(text, 4426).placed(), "{text} was refused");
        assert_eq!(seat.read(), text, "the system clipboard did not take {text}");
    }
}

#[test]
fn the_copy_a_focused_client_makes_releases_before_it_claims() {
    let mut seat = FakeSeat::new();
    seat.copy_text(FOUR[0], 4426);
    seat.copy_text(FOUR[1], 4426);
    assert_eq!(seat.wire, [
        Step::SetSelection { source: 1, serial: 4426 },
        Step::Destroy(1),
        Step::SetSelection { source: 2, serial: 4426 },
    ]);
}

#[test]
fn an_unfocused_copy_keeps_the_selection_it_holds_instead_of_walking_the_clipboard_backwards() {
    let mut seat = FakeSeat::new();
    seat.copy_text("FIX-01-aaaa", 4426);
    seat.copy_text("FIX-02-bbbb", 4426);
    seat.focused = false;
    let sent = seat.wire.len();
    let outcome = seat.copy_text("FIX-03-cccc", 4426);
    assert_eq!(seat.read(), "FIX-03-cccc", "the clipboard went back to the previous text");
    assert_eq!(outcome, CopyOutcome::Kept);
    assert_eq!(seat.wire.len(), sent, "an unfocused copy must send nothing at all");
}

#[test]
fn an_unfocused_copy_that_holds_no_selection_reports_the_refusal_rather_than_success() {
    let mut seat = FakeSeat::new();
    seat.focused = false;
    seat.cached = "THIRD-PARTY".to_owned();
    let outcome = seat.copy_text("FIX-04-dddd", 4426);
    assert_eq!(seat.read(), "THIRD-PARTY", "the clipboard did not move, whatever we answered");
    assert!(!outcome.placed(), "a copy the compositor drops was reported as success");
    assert_eq!(outcome, CopyOutcome::Refused(CopyRefusal::Unfocused));
    assert!(seat.wire.is_empty(), "a claim the compositor drops is not worth a source");
}

#[test]
fn a_selection_the_compositor_binds_no_device_for_is_refused_and_placed_says_so() {
    let mut seat = FakeSeat::new();
    seat.device = false;
    assert_eq!(seat.copy_text("ZULU-00-aaaa", 4426), CopyOutcome::Refused(CopyRefusal::NoDevice));
    assert!(CopyOutcome::Claimed.placed());
    assert!(CopyOutcome::Kept.placed());
    assert!(!CopyOutcome::Refused(CopyRefusal::NoSerial).placed());
}

#[test]
fn a_copy_off_the_loop_thread_answers_the_loop_s_verdict_and_never_guesses() {
    let (ack, verdict) = mpsc::channel();
    ack.send(CopyOutcome::Refused(CopyRefusal::Unfocused)).unwrap();
    assert!(!copy_verdict(&verdict));
    ack.send(CopyOutcome::Kept).unwrap();
    assert!(copy_verdict(&verdict));
    drop(ack);
    assert!(!copy_verdict(&verdict));
}

#[test]
fn a_copy_with_no_loop_left_to_answer_it_is_false_and_leaves_no_own_slot_behind() {
    let (sender, receiver) = mpsc::channel();
    let shared = Arc::new(Mutex::new(Shared::default()));
    let loop_thread = std::thread::spawn(|| std::thread::current().id()).join().unwrap();
    let waker = Waker::new().expect("eventfd");
    let handle = WaylandClipboard::new(sender, waker, Arc::clone(&shared), loop_thread);
    drop(receiver);
    assert!(!handle.copy(WaylandSelection::Clipboard, "hello"));
    assert_eq!(handle.paste_owned(WaylandSelection::Clipboard), None);
}

#[test]
fn a_paste_prefers_utf8_then_the_legacy_text_types() {
    let s = |v: &[&str]| v.iter().map(|m| (*m).to_owned()).collect::<Vec<_>>();
    assert_eq!(
        pick_text_mime(&s(&["image/png", "STRING", "text/plain;charset=utf-8"])).as_deref(),
        Some("text/plain;charset=utf-8")
    );
    assert_eq!(pick_text_mime(&s(&["TEXT", "STRING"])).as_deref(), Some("TEXT"));
    assert_eq!(pick_text_mime(&s(&["image/png"])), None);
    assert_eq!(pick_text_mime(&[]), None);
}

#[test]
fn the_own_slot_answers_synchronously_and_a_foreign_slot_answers_none() {
    let (sender, _receiver) = mpsc::channel();
    let shared = Arc::new(Mutex::new(Shared::default()));
    let waker = Waker::new().expect("eventfd");
    let handle =
        WaylandClipboard::new(sender, waker, Arc::clone(&shared), std::thread::current().id());
    assert_eq!(handle.paste_owned(WaylandSelection::Clipboard), None);
    assert!(handle.copy(WaylandSelection::Clipboard, "hello"));
    assert_eq!(handle.paste_owned(WaylandSelection::Clipboard).as_deref(), Some("hello"));
    assert_eq!(handle.paste_owned(WaylandSelection::Primary), None);
    assert_eq!(handle.paste(WaylandSelection::Primary), None);
    shared.lock().unwrap().clipboard = None;
    assert_eq!(handle.paste_owned(WaylandSelection::Clipboard), None);
}
