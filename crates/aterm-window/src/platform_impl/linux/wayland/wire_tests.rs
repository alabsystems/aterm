// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

use super::*;

#[test]
fn an_array_is_native_words_and_a_partial_word_is_left_out() {
    let mut bytes = Vec::new();
    for w in [1u32, 4, 0xdead_beef] {
        bytes.extend(w.to_ne_bytes());
    }
    bytes.push(7);
    assert_eq!(words(&bytes), [1, 4, 0xdead_beef]);
}

/// What a wire is, for an order check.
fn label(wire: &Wire) -> &'static str {
    use aterm_wayland::seat::KeyboardEvent;
    match wire {
        Wire::Seat(Input::Keyboard { event: KeyboardEvent::Enter { .. }, .. }) => "focus",
        Wire::Seat(Input::Keyboard { .. }) => "key",
        Wire::Seat(Input::Pointer { .. }) => "motion",
        Wire::Seat(_) => "capabilities",
        Wire::TextInput { event: TextInputEvent::Enter(_), .. } => "ime-enter",
        Wire::TextInput { event: TextInputEvent::CommitString(_), .. } => "commit",
        Wire::TextInput { event: TextInputEvent::Done, .. } => "done",
        Wire::TextInput { .. } => "text-input",
        Wire::PreferredScale { .. } => "scale",
        Wire::OutputDone(_) => "output",
        Wire::Drag { .. } => "drop",
        Wire::Source { .. } => "cancel",
        _ => "other",
    }
}

/// The sinks aterm-wayland's objects are given.
struct Sinks {
    outputs: Sink<OutputInfo>,
    seats: Sink<Input>,
    drags: Sink<DragEvent>,
    source: Sink<SourceEvent>,
}

impl Sinks {
    fn of(inbox: &Inbox) -> Sinks {
        Sinks {
            outputs: inbox.output_sink(),
            seats: inbox.seat_sink(),
            drags: inbox.drag_sink(7),
            source: inbox.source_sink(WaylandSelection::Clipboard, 1),
        }
    }
}

/// One dispatch, as the listeners hear it: the seat's keyboard focus and then the text
/// input's `enter` (Mutter's order); BackSpace (evdev 14) and then the input method's commit
/// of "，"; a pointer motion and then the window's new preferred scale; an output's `done`;
/// a drop and then the seat's capabilities; a source's cancel. The seat's, the output's, the
/// drag's and the source's go through the sinks aterm-wayland's objects are given; the text
/// input's and the scale's are the backend's own listeners' (`Owned::listened` pushes what
/// they decode).
fn dispatch(own: &Inbox, sinks: &Sinks) {
    use aterm_wayland::seat::KeyboardEvent;
    let keyboard = |event| Input::Keyboard { seat: 7, event };
    let text_input = |event| Wire::TextInput { seat: 7, event };
    (sinks.seats)(keyboard(KeyboardEvent::Enter { serial: 1, surface: None, keys: Vec::new() }));
    own.push(text_input(TextInputEvent::Enter(None)));
    (sinks.seats)(keyboard(KeyboardEvent::Key { serial: 2, time: 10, key: 14, state: 1 }));
    own.push(text_input(TextInputEvent::CommitString(Some("，".into()))));
    own.push(text_input(TextInputEvent::Done));
    (sinks.seats)(Input::Pointer { seat: 7, frame: Vec::new() });
    own.push(Wire::PreferredScale { window: WindowId(1), scale: 180 });
    (sinks.outputs)(OutputInfo { global: 3, scale: 2, ..OutputInfo::default() });
    (sinks.drags)(DragEvent::Drop);
    (sinks.seats)(Input::Capabilities { seat: 7, capabilities: 0 });
    (sinks.source)(SourceEvent::Cancelled);
}

const AS_DISPATCHED: [&str; 11] = [
    "focus",
    "ime-enter",
    "key",
    "commit",
    "done",
    "motion",
    "scale",
    "output",
    "drop",
    "capabilities",
    "cancel",
];

#[test]
fn every_object_s_events_are_applied_in_the_order_they_were_dispatched() {
    let inbox = Inbox::default();
    dispatch(&inbox, &Sinks::of(&inbox));
    let applied: Vec<&str> = inbox.take().iter().map(label).collect();
    assert_eq!(applied, AS_DISPATCHED);

    let own = Inbox::default();
    let books: [Inbox; 4] = Default::default();
    let [seats, drags, sources, outputs] = &books;
    let sinks = Sinks {
        outputs: outputs.output_sink(),
        seats: seats.seat_sink(),
        drags: drags.drag_sink(7),
        source: sources.source_sink(WaylandSelection::Clipboard, 1),
    };
    dispatch(&own, &sinks);
    let stream_by_stream: Vec<&str> = std::iter::once(&own)
        .chain(&books)
        .flat_map(Inbox::take)
        .map(|wire| label(&wire))
        .collect();
    assert_eq!(stream_by_stream, [
        "ime-enter",
        "commit",
        "done",
        "scale",
        "focus",
        "key",
        "motion",
        "capabilities",
        "drop",
        "cancel",
        "output"
    ]);
    assert_ne!(stream_by_stream, AS_DISPATCHED);
}

#[test]
fn the_inbox_hands_out_what_it_heard_in_order_once() {
    let inbox = Inbox::default();
    let listener = inbox.clone();
    listener.push(Wire::PreferredScale { window: WindowId(1), scale: 120 });
    listener.push(Wire::PreferredScale { window: WindowId(2), scale: 180 });
    let got = inbox.take();
    assert!(matches!(got.as_slice(), [
        Wire::PreferredScale { window: WindowId(1), scale: 120 },
        Wire::PreferredScale { window: WindowId(2), scale: 180 }
    ]));
    assert!(inbox.take().is_empty());
}
