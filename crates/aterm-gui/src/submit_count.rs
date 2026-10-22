// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE SESSION'S SUBMISSIONS (`text --json`'s `"submits"`): how many times an
//! Enter reached the session's program over something typed into it since the
//! Enter before, whoever typed it — a person at the window (their keys, a
//! paste, an IME commit, a development build's `as=person`) or a control
//! client (`send`, `key`, `paste`, `feed`, `turn`: the harness's own acts, a
//! lab's prompt, an orchestrator's).
//!
//! What it is for: the harness tells one ended turn from the next by what it
//! READS (the message that opened the turn, the words that end it), and a
//! turn answered between two of its reads, opened by the same words as the
//! one before or answered with the very words of the reply before it, reads
//! as the point it came after (P4 of
//! `docs/measured/forever-lab-2026-10-20.md`). aterm writes every byte typed
//! into the tab, so it knows a submission happened whatever the screen shows:
//! a count that moved since the point the harness judged last says a new turn
//! was asked, and one that did not says the same words are the same turn.
//!
//! **What counts** ([`SubmitInput::of`]): a plain Enter press (no Shift,
//! Ctrl, Alt or Super held; keypad Enter too), or a carriage return in raw
//! bytes, written after at least one composing input since the last one: a
//! key press that is no plain Enter (a character, Backspace, an arrow,
//! Shift+Enter's newline), text, a paste, a raw byte that is no CR. An Enter
//! over nothing typed since the last (a second Enter, a lone Enter at an empty
//! composer) is no submission. A key release, a modifier or lock key alone, a
//! mouse report, a focus or colour report and an engine reply compose
//! nothing, and only the egress law's writes are read
//! (`crate::app_input::OutputEchoTracker`), so reports never reach it.
//!
//! **When it counts**: at the write's full acceptance by the session's sink
//! (`EgressReceipt::fully_accepted_nonempty`), before the program can have
//! read the bytes, so a read of the screen that shows the program's answer to
//! a submission, followed by a read of this count, counts that submission.
//! A write the sink refused moved nothing and counts nothing.
//!
//! **What it is not**: a reading of the composer. aterm reads no composer at
//! the moment of the key (the press path takes no terminal lock), so a draft
//! typed and then deleted before its Enter, or an Esc before an Enter on an
//! empty composer, counts as a submission the program answered with nothing.
//! A reader pairs the count with the screen it reads, and a count that moved
//! while the screen stands as it did is a submission not drawn yet, or none.
//!
//! Lock-free: one relaxed-to-acquire flag and one counter per session, written
//! once per accepted input write. The count lives in this process: a session
//! carried across a seamless update starts again at `0`, as `human_seq` does.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::input::InputEvent;

/// One session's submission count ([`Self::submits`]) and whether anything
/// was composed since the last Enter. Lives on
/// `crate::app_input::OutputEchoTracker`, beside its count of input writes.
#[derive(Debug, Default)]
pub(crate) struct SubmitCount {
    /// A composing input was accepted since the last Enter.
    composed: AtomicBool,
    /// How many Enters submitted something composed.
    submits: AtomicU64,
}

/// What one input write means for the count ([`SubmitInput::of`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum SubmitInput {
    /// Composes nothing and submits nothing: a key release, a modifier or
    /// lock key alone, a mouse or focus event, empty bytes.
    #[default]
    Neither,
    /// Composes: a key press that is no plain Enter, text, a paste.
    Composes,
    /// A plain Enter press: submits what was composed since the last.
    Enter,
    /// Raw bytes holding `crs` carriage returns: `lead` is whether a byte
    /// that is no CR comes before the first, `inner` how many of the later
    /// ones follow such a byte since the CR before, `tail` whether one comes
    /// after the last.
    Raw {
        lead: bool,
        crs: u32,
        inner: u32,
        tail: bool,
    },
}

impl SubmitInput {
    /// The class of `ev` (module header, "What counts").
    pub(crate) fn of(ev: &InputEvent) -> Self {
        use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
        match ev {
            InputEvent::Key {
                event_type: KeyEventType::Release,
                ..
            } => Self::Neither,
            InputEvent::Key { key, mods, .. } => {
                let folded = ev.keypad_folded();
                let key = match folded.as_ref() {
                    Some(InputEvent::Key { key, .. }) => key,
                    _ => key,
                };
                match key {
                    Key::Named(NamedKey::Enter)
                        if !mods.intersects(
                            Modifiers::SHIFT | Modifiers::CTRL | Modifiers::ALT | Modifiers::SUPER,
                        ) =>
                    {
                        Self::Enter
                    }
                    Key::Named(NamedKey::CapsLock | NamedKey::NumLock | NamedKey::ScrollLock) => {
                        Self::Neither
                    }
                    Key::Named(named) if named.modifier_twin().is_some() => Self::Neither,
                    _ => Self::Composes,
                }
            }
            InputEvent::Text(text) => Self::raw(text.as_bytes()),
            InputEvent::KeySequence(bytes) => Self::raw(bytes),
            InputEvent::Paste(text, _) if !text.is_empty() => Self::Composes,
            _ => Self::Neither,
        }
    }

    /// The class of raw `bytes` written verbatim (`send`, `feed`, a
    /// `[key_sequences]` chord, committed text).
    fn raw(bytes: &[u8]) -> Self {
        let mut lead = false;
        let mut crs = 0_u32;
        let mut inner = 0_u32;
        let mut run = false;
        for &b in bytes {
            if b == b'\r' {
                if crs == 0 {
                    lead = run;
                } else if run {
                    inner = inner.saturating_add(1);
                }
                crs = crs.saturating_add(1);
                run = false;
            } else {
                run = true;
            }
        }
        match (crs, run) {
            (0, false) => Self::Neither,
            (0, true) => Self::Composes,
            _ => Self::Raw {
                lead,
                crs,
                inner,
                tail: run,
            },
        }
    }
}

impl SubmitCount {
    /// An input write of class `input` was fully accepted by the session's
    /// sink: fold it into the count.
    pub(crate) fn accepted(&self, input: SubmitInput) {
        match input {
            SubmitInput::Neither => {}
            SubmitInput::Composes => self.composed.store(true, Ordering::Release),
            SubmitInput::Enter => {
                if self.composed.swap(false, Ordering::AcqRel) {
                    self.submits.fetch_add(1, Ordering::AcqRel);
                }
            }
            SubmitInput::Raw {
                lead,
                crs,
                inner,
                tail,
            } => {
                if crs == 0 {
                    self.composed.fetch_or(tail, Ordering::AcqRel);
                    return;
                }
                let before = self.composed.swap(tail, Ordering::AcqRel);
                let n = u64::from(lead || before) + u64::from(inner);
                if n > 0 {
                    self.submits.fetch_add(n, Ordering::AcqRel);
                }
            }
        }
    }

    /// How many submissions the session's program has been written in this
    /// process (`text --json`'s `"submits"`).
    pub(crate) fn submits(&self) -> u64 {
        self.submits.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::{SubmitCount, SubmitInput};
    use crate::input::{InputEvent, PasteFraming};
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

    fn key(key: Key, mods: Modifiers, event_type: KeyEventType) -> InputEvent {
        InputEvent::Key {
            key,
            mods,
            base_layout: None,
            layout_text: None,
            event_type,
        }
    }

    fn press(k: Key) -> InputEvent {
        key(k, Modifiers::empty(), KeyEventType::Press)
    }

    fn enter() -> InputEvent {
        press(Key::Named(NamedKey::Enter))
    }

    fn written(count: &SubmitCount, events: &[InputEvent]) -> u64 {
        for ev in events {
            count.accepted(SubmitInput::of(ev));
        }
        count.submits()
    }

    #[test]
    fn an_enter_over_something_typed_is_a_submission_whoever_typed_it() {
        // A person's keys, then Enter.
        let count = SubmitCount::default();
        let typed = [
            press(Key::Character('h')),
            press(Key::Character('i')),
            enter(),
        ];
        assert_eq!(written(&count, &typed), 1);
        // A control client's `send` then `key enter` (the harness's act, a lab's prompt).
        let sent = [InputEvent::KeySequence(b"keep going".to_vec()), enter()];
        assert_eq!(written(&count, &sent), 2);
        // A `turn`'s paste then its Enter.
        let turn = [
            InputEvent::Paste("land the task".to_string(), PasteFraming::AtDrain),
            enter(),
        ];
        assert_eq!(written(&count, &turn), 3);
        // Raw bytes that carry their own CR, one submission per line typed.
        let raw = [InputEvent::KeySequence(b"one\rtwo\r".to_vec())];
        assert_eq!(written(&count, &raw), 5);
        // History recalled with an arrow, then Enter: the arrow composed.
        let recalled = [press(Key::Named(NamedKey::ArrowUp)), enter()];
        assert_eq!(written(&count, &recalled), 6);
        // Keypad Enter submits as Enter does.
        let keypad = [
            press(Key::Character('x')),
            press(Key::Named(NamedKey::NumpadEnter)),
        ];
        assert_eq!(written(&count, &keypad), 7);
    }

    #[test]
    fn an_enter_over_nothing_typed_and_a_newline_are_no_submission() {
        let count = SubmitCount::default();
        // A lone Enter, a second Enter, a bare CR.
        let empty = [enter(), press(Key::Character('a')), enter(), enter()];
        assert_eq!(
            written(&count, &empty),
            1,
            "the second Enter submits nothing"
        );
        assert_eq!(
            written(&count, &[InputEvent::KeySequence(b"\r".to_vec())]),
            1
        );
        // Shift+Enter is the composer's newline: it composes, and submits nothing.
        let shifted = key(
            Key::Named(NamedKey::Enter),
            Modifiers::SHIFT,
            KeyEventType::Press,
        );
        assert_eq!(written(&count, &[shifted]), 1);
        assert_eq!(written(&count, &[enter()]), 2, "the newline was composed");
        // A release, a modifier alone and a lock key compose nothing.
        let inert = [
            key(
                Key::Character('a'),
                Modifiers::empty(),
                KeyEventType::Release,
            ),
            press(Key::Named(NamedKey::ShiftLeft)),
            press(Key::Named(NamedKey::CapsLock)),
            enter(),
        ];
        assert_eq!(written(&count, &inert), 2);
        // Nor does a mouse report or a focus event.
        let reports = [InputEvent::Focus(true), enter()];
        assert_eq!(written(&count, &reports), 2);
        // A Enter's own release, under kitty's event-type reports, composes
        // nothing: the Enter after it is over an empty composer.
        let release = key(
            Key::Named(NamedKey::Enter),
            Modifiers::empty(),
            KeyEventType::Release,
        );
        assert_eq!(written(&count, &[release, enter()]), 2);
    }

    #[test]
    fn raw_bytes_carry_their_composing_across_writes() {
        let count = SubmitCount::default();
        // Text sent with no CR composes; the CR of the next write submits it.
        assert_eq!(
            written(&count, &[InputEvent::KeySequence(b"draft".to_vec())]),
            0
        );
        assert_eq!(
            written(&count, &[InputEvent::KeySequence(b"\r".to_vec())]),
            1
        );
        // Text after the last CR composes for the next Enter.
        assert_eq!(
            written(&count, &[InputEvent::KeySequence(b"a\rb".to_vec())]),
            2
        );
        assert_eq!(written(&count, &[enter()]), 3);
        // Blank lines between CRs are no submissions.
        assert_eq!(
            written(&count, &[InputEvent::KeySequence(b"\r\r\r".to_vec())]),
            3
        );
    }
}
