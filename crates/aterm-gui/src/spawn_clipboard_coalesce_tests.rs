// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_core::terminal::ClipboardSelection as Sel;
use aterm_models_gui::clipboard_mailbox_model;
use aterm_spec::derive::Model;

use super::{
    ClipMailboxReceiver, ClipWrite, QuerySource, clip_mailbox, route_osc52_query, route_osc52_write,
};

fn set(clipboard: Option<&str>, primary: Option<&str>) -> ClipWrite {
    ClipWrite {
        clipboard: clipboard.map(From::from),
        primary: primary.map(From::from),
    }
}

fn assert_mailbox_projection(
    model: &Model,
    state: &aterm_spec::interp::State,
    receiver: &ClipMailboxReceiver,
    clipboard_generation: i64,
    primary_generation: i64,
) {
    let actual = receiver
        .mailbox
        .state
        .lock()
        .expect("clipboard mailbox lock");
    assert_eq!(state[&"pending"], i64::from(actual.pending.is_some()));
    assert_eq!(state[&"clipboard_generation"], clipboard_generation);
    assert_eq!(state[&"primary_generation"], primary_generation);
    let pending_clipboard_present = actual
        .pending
        .as_ref()
        .is_some_and(|pending| pending.clipboard.is_some());
    let pending_primary_present = actual
        .pending
        .as_ref()
        .is_some_and(|pending| pending.primary.is_some());
    assert_eq!(
        state[&"pending_clipboard_present"],
        i64::from(pending_clipboard_present)
    );
    assert_eq!(
        state[&"pending_primary_present"],
        i64::from(pending_primary_present)
    );
    if let Some(pending) = actual.pending.as_ref() {
        if let Some(generation) = pending.clipboard.as_deref() {
            assert_eq!(
                state[&"pending_clipboard"],
                generation
                    .parse::<i64>()
                    .expect("numeric clipboard generation")
            );
        }
        if let Some(generation) = pending.primary.as_deref() {
            assert_eq!(
                state[&"pending_primary"],
                generation
                    .parse::<i64>()
                    .expect("numeric PRIMARY generation")
            );
        }
    }
    assert!(model.check_invariant("OneLatestPendingWrite", state));
}

#[test]
fn osc52_writes_and_reads_the_slot_its_target_names() {
    type Slots<'a> = (Option<&'a str>, Option<&'a str>);
    let cases: [(&[Sel], bool, Slots, QuerySource); 6] = [
        (
            &[Sel::Clipboard],
            true,
            (Some("x"), None),
            QuerySource::Clipboard,
        ),
        (&[], true, (Some("x"), None), QuerySource::Clipboard),
        (
            &[Sel::Clipboard, Sel::Primary],
            true,
            (Some("x"), Some("x")),
            QuerySource::Clipboard,
        ),
        (
            &[Sel::Primary],
            true,
            (None, Some("x")),
            QuerySource::Primary,
        ),
        (
            &[Sel::Primary],
            false,
            (Some("x"), None),
            QuerySource::Clipboard,
        ),
        (
            &[Sel::Clipboard, Sel::Primary],
            false,
            (Some("x"), None),
            QuerySource::Clipboard,
        ),
    ];
    for (sels, x11, slots, source) in cases {
        let w = route_osc52_write(sels, "x", x11);
        assert_eq!(
            (w.clipboard.as_deref(), w.primary.as_deref()),
            slots,
            "{sels:?} x11={x11}"
        );
        assert_eq!(route_osc52_query(sels, x11), source, "{sels:?} x11={x11}");
    }
}

#[test]
fn mailbox_drains_final_write_then_closes() {
    let (tx, rx) = clip_mailbox();
    tx.publish(set(Some("only"), None));
    drop(tx);
    let folded = rx.recv().expect("retained final write");
    assert_eq!(
        (folded.clipboard.as_deref(), folded.primary),
        (Some("only"), None)
    );
    assert!(rx.recv().is_none());
}

#[test]
fn real_mailbox_conforms_to_the_derived_latest_slot_model() {
    let model = clipboard_mailbox_model();
    let mut state = model.init_state();
    let (tx, rx) = clip_mailbox();

    assert!(model.action_enabled("PublishClipboard", &state));
    tx.publish(set(Some("1"), None));
    assert!(model.fire("PublishClipboard", &mut state));
    assert_mailbox_projection(&model, &state, &rx, 1, 0);

    assert!(model.action_enabled("PublishPrimary", &state));
    tx.publish(set(None, Some("1")));
    assert!(model.fire("PublishPrimary", &mut state));
    assert_mailbox_projection(&model, &state, &rx, 1, 1);

    assert!(model.action_enabled("PublishClipboard", &state));
    tx.publish(set(Some("2"), None));
    assert!(model.fire("PublishClipboard", &mut state));
    assert_mailbox_projection(&model, &state, &rx, 2, 1);

    assert!(model.action_enabled("Consume", &state));
    let delivered = rx.recv().expect("one coalesced write");
    assert_eq!(delivered.clipboard.as_deref(), Some("2"));
    assert_eq!(delivered.primary.as_deref(), Some("1"));
    assert!(model.fire("Consume", &mut state));
    assert_mailbox_projection(&model, &state, &rx, 2, 1);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut retired = buggy.init_state();
    assert!(buggy.fire("PublishClipboard", &mut retired));
    assert!(buggy.check_invariant("OneLatestPendingWrite", &retired));
    assert!(buggy.fire("PublishClipboard", &mut retired));
    assert!(!buggy.check_invariant("OneLatestPendingWrite", &retired));
}
