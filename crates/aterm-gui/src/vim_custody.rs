// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Vim composer evidence shared by input and the frame that can display its echo.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, TryLockError};
use std::time::Instant;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Mode {
    #[default]
    Unknown,
    Insert,
    Normal,
    ExitHint,
}

/// A complete prompt frame's mode; an absent or partial footer establishes nothing.
pub(crate) fn mode(rows: &[String], caret_row: usize) -> Mode {
    if !aterm_phase::phase::prompt_box_holds(rows, caret_row) {
        return Mode::Unknown;
    }
    let Some(bottom) = aterm_phase::phase::prompt_box_bottom(rows) else {
        return Mode::Unknown;
    };
    for row in rows[bottom + 1..]
        .iter()
        .take_while(|row| !row.trim().is_empty())
    {
        let row = row.trim_start();
        if row.starts_with(aterm_phase::anchor!("vim.insert")) {
            return Mode::Insert;
        }
        if row.starts_with("Press Ctrl-C again to exit") {
            return Mode::ExitHint;
        }
        if row.starts_with(['⏵', '⏸'])
            || row.starts_with("? for shortcuts")
            || row.starts_with("! for shell mode")
        {
            return Mode::Normal;
        }
    }
    Mode::Unknown
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Owner {
    pub terminal: u64,
    pub foreground: crate::session_timeline::ForegroundStamp,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Frame {
    pub owner: Owner,
    pub generation: u64,
    pub epoch: u64,
    pub mode: Mode,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Key {
    Text,
    Entry(char),
    Escape,
    Boundary,
}

fn has_escape(bytes: &[u8]) -> bool {
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != 0x1b {
            at += 1;
            continue;
        }
        match bytes.get(at + 1) {
            Some(b'[') => {
                let mut end = at + 2;
                while bytes
                    .get(end)
                    .is_some_and(|byte| (0x20..=0x3f).contains(byte))
                {
                    end += 1;
                }
                if !bytes
                    .get(end)
                    .is_some_and(|byte| (0x40..=0x7e).contains(byte))
                {
                    return true;
                }
                at = end + 1;
            }
            Some(b'O') if bytes.get(at + 2).is_some_and(|b| (0x40..=0x7e).contains(b)) => {
                at += 3;
            }
            _ => return true,
        }
    }
    false
}

pub(crate) fn key(event: &crate::input::InputEvent, text: bool) -> Key {
    use crate::input::InputEvent;
    use aterm_types::keyboard::{Key as K, Modifiers, NamedKey};
    let escape = match event {
        InputEvent::Key {
            key: K::Named(NamedKey::Escape),
            ..
        } => true,
        InputEvent::Key {
            key: K::Character('['),
            mods,
            ..
        } => mods.contains(Modifiers::CTRL),
        InputEvent::KeySequence(bytes) => has_escape(bytes),
        InputEvent::Text(text) => has_escape(text.as_bytes()),
        _ => false,
    };
    let entry = match event {
        InputEvent::Key {
            key: K::Character(c @ ('i' | 'a')),
            mods,
            ..
        } if !mods.intersects(Modifiers::CTRL | Modifiers::ALT | Modifiers::SUPER) => Some(*c),
        InputEvent::Text(s) if s == "i" || s == "a" => s.chars().next(),
        InputEvent::KeySequence(bytes) if bytes == b"i" || bytes == b"a" => {
            Some(char::from(bytes[0]))
        }
        _ => None,
    };
    if escape {
        Key::Escape
    } else if let Some(glyph) = entry {
        Key::Entry(glyph)
    } else if text {
        Key::Text
    } else {
        Key::Boundary
    }
}

#[derive(Debug, Default)]
struct State {
    owner: Option<Owner>,
    epoch: u64,
    generation: u64,
    mode: Mode,
    known: bool,
    require_normal: bool,
    boundary: Option<u64>,
    editable: bool,
    hint_editable: bool,
    hint_entry: Option<Instant>,
    entry: Option<(Instant, char)>,
}

impl State {
    fn frame(&mut self, frame: Frame) -> bool {
        if self.owner != Some(frame.owner) {
            *self = Self {
                owner: Some(frame.owner),
                epoch: self.epoch,
                boundary: Some(frame.generation),
                ..Self::default()
            };
        }
        if frame.generation < self.generation {
            return false;
        }
        let newer = self.boundary.is_none_or(|before| frame.generation > before);
        self.generation = frame.generation;
        match frame.mode {
            Mode::Insert => {
                self.known = true;
                self.editable = !self.require_normal && newer;
                if self.editable {
                    self.hint_entry = None;
                }
            }
            Mode::Normal if self.known => {
                if newer {
                    self.require_normal = false;
                }
                self.editable = false;
                self.hint_editable = false;
                self.hint_entry = None;
            }
            Mode::ExitHint => self.editable = self.hint_editable && !self.require_normal && newer,
            Mode::Normal | Mode::Unknown => self.editable = false,
        }
        self.mode = frame.mode;
        self.known && self.editable
    }

    #[cfg(test)]
    fn key(&mut self, frame: Frame, key: Key) -> bool {
        self.key_at(frame, key, Instant::now())
    }

    fn key_at(&mut self, frame: Frame, key: Key, at: Instant) -> bool {
        self.frame(frame);
        match key {
            Key::Escape | Key::Boundary => {
                // An entry command the agent has not drawn yet (`i` on a NORMAL frame, then
                // Ctrl-C before the repaint) puts it in INSERT as surely as the indicator does,
                // and Claude stays in INSERT across Ctrl-C under its exit hint.
                let entry_at = self.entry.take().map(|(at, _)| at);
                let inserting = self.mode == Mode::Insert || entry_at.is_some();
                self.hint_editable = self.known && inserting && !self.require_normal;
                self.hint_entry = if self.hint_editable && self.mode != Mode::Insert {
                    entry_at
                } else {
                    None
                };
                if matches!(key, Key::Escape) {
                    self.require_normal = self.known && self.mode != Mode::Normal;
                    self.hint_editable = false;
                    self.hint_entry = None;
                }
                self.boundary = Some(frame.generation);
                self.editable = false;
            }
            Key::Entry(glyph)
                if self.known && self.mode == Mode::Normal && !self.require_normal =>
            {
                self.entry.get_or_insert((at, glyph));
            }
            Key::Text | Key::Entry(_) => {}
        }
        self.known && self.editable && !self.require_normal
    }
}

/// A missed contended event invalidates old evidence; input itself never waits here.
#[derive(Debug, Default)]
pub(crate) struct Custody {
    epoch: AtomicU64,
    known: AtomicBool,
    state: Mutex<State>,
}

impl Custody {
    fn cached_mode(&self, owner: Owner, generation: u64) -> Option<Mode> {
        let state = self.state.try_lock().ok()?;
        (state.owner == Some(owner) && state.generation == generation).then_some(state.mode)
    }
    pub(crate) fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    pub(crate) fn invalidate(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }
    pub(crate) fn known(&self) -> bool {
        self.known.load(Ordering::Acquire)
    }

    pub(crate) fn key(&self, frame: Frame, key: Key) -> bool {
        self.key_at(frame, key, Instant::now())
    }

    pub(crate) fn key_at(&self, frame: Frame, key: Key, at: Instant) -> bool {
        let epoch = self.epoch.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        let mut state = match self.state.try_lock() {
            Ok(state) => state,
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
            Err(TryLockError::WouldBlock) => return false,
        };
        if frame.epoch.wrapping_add(1) != epoch || state.epoch.wrapping_add(1) != epoch {
            *state = State {
                owner: Some(frame.owner),
                require_normal: true,
                known: self.known(),
                ..State::default()
            };
        }
        state.epoch = epoch;
        let answer = state.key_at(frame, key, at);
        self.known.store(state.known, Ordering::Release);
        answer && self.epoch() == epoch
    }

    pub(crate) fn entry_at(&self, frame: Frame) -> Option<Instant> {
        let state = self.state.try_lock().ok()?;
        (self.epoch() == frame.epoch && state.owner == Some(frame.owner))
            .then_some(state.entry)
            .flatten()
            .map(|(at, _)| at)
    }

    /// A queued entry command that failed before delivery cannot prove a mode transition.
    pub(crate) fn reject_entry(&self, at: Instant) {
        let Ok(mut state) = self.state.try_lock() else {
            self.invalidate();
            return;
        };
        let mut revoked = false;
        if state.entry.is_some_and(|(entry, _)| entry == at) {
            state.entry = None;
            revoked = true;
        }
        if state.hint_entry == Some(at) {
            state.hint_entry = None;
            state.hint_editable = false;
            state.editable = false;
            revoked = true;
        }
        if revoked {
            state.epoch = self.epoch.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        }
    }

    pub(crate) fn frame(&self, frame: Frame) -> bool {
        let epoch = self.epoch.load(Ordering::Acquire);
        if frame.epoch != epoch {
            return false;
        }
        let mut state = match self.state.try_lock() {
            Ok(state) => state,
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
            Err(TryLockError::WouldBlock) => return false,
        };
        if state.epoch != epoch {
            *state = State {
                owner: Some(frame.owner),
                epoch,
                require_normal: true,
                known: self.known(),
                ..State::default()
            };
        }
        let answer = state.frame(frame);
        self.known.store(state.known, Ordering::Release);
        answer && self.epoch() == epoch
    }
}

/// Read only the terminal already held by the caller and retained owner metadata.
pub(crate) fn capture(
    terminal: &aterm_core::terminal::Terminal,
    context: &crate::SessionCtx,
    discover: bool,
) -> Option<Frame> {
    capture_parts(terminal, &context.licence, &context.timeline, discover)
}

pub(crate) fn capture_parts(
    terminal: &aterm_core::terminal::Terminal,
    licence: &crate::licence_fence::LicenceFence,
    timeline: &Mutex<crate::session_timeline::SessionTimeline>,
    discover: bool,
) -> Option<Frame> {
    let custody = &licence.vim;
    if !discover && !custody.known() {
        return None;
    }
    let epoch = custody.epoch();
    let foreground = timeline.try_lock().ok()?.foreground_stamp()?;
    let owner = Owner {
        terminal: terminal.render_identity(),
        foreground,
    };
    let generation = terminal.screen_mutations();
    let mode = custody.cached_mode(owner, generation).unwrap_or_else(|| {
        let rows: Vec<_> = (0..usize::from(terminal.rows()))
            .map(|r| crate::claude_lights::row_text(terminal, r))
            .collect();
        mode(&rows, usize::from(terminal.cursor().row))
    });
    (custody.epoch() == epoch).then_some(Frame {
        owner,
        generation,
        epoch,
        mode,
    })
}

/// Revalidate the captured source immediately before its evidence is applied.
pub(crate) fn still_current(
    frame: Frame,
    terminal: &std::sync::Arc<Mutex<aterm_core::terminal::Terminal>>,
    context: &crate::SessionCtx,
) -> bool {
    context.licence.vim.epoch() == frame.epoch
        && terminal.try_lock().ok().is_some_and(|terminal| {
            terminal.render_identity() == frame.owner.terminal
                && context
                    .timeline
                    .try_lock()
                    .ok()
                    .and_then(|tl| tl.foreground_stamp())
                    == Some(frame.owner.foreground)
                && context.licence.vim.epoch() == frame.epoch
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_navigation_sequences_do_not_become_standalone_escape_keys() {
        assert!(!has_escape(b"\x1b[A\x1b[1;5D\x1bOF"));
        assert!(has_escape(b"\x1b"));
        assert!(has_escape(b"\x1bp"));
        assert!(has_escape(b"\x1b[A\x1b"));
    }

    fn frame(terminal: u64, generation: u64, mode: Mode) -> Frame {
        let mut timeline = crate::session_timeline::SessionTimeline::default();
        timeline.note_foreground_group(123);
        timeline.set_program(123, Some("claude".into()));
        Frame {
            owner: Owner {
                terminal,
                foreground: timeline.foreground_stamp().unwrap(),
            },
            generation,
            epoch: 0,
            mode,
        }
    }

    fn inserting() -> State {
        let mut custody = State::default();
        let insert = frame(1, 1, Mode::Insert);
        assert!(!custody.key(insert, Key::Text));
        assert!(custody.frame(frame(1, 2, Mode::Insert)));
        custody
    }

    #[test]
    fn first_queued_text_waits_for_insert_after_a_confirmed_normal_frame() {
        let mut custody = inserting();
        assert!(!custody.key(frame(1, 2, Mode::Insert), Key::Escape));
        assert!(!custody.frame(frame(1, 3, Mode::Normal)));
        assert!(!custody.key(frame(1, 3, Mode::Normal), Key::Text)); // i
        assert!(!custody.key(frame(1, 3, Mode::Normal), Key::Text)); // p
        assert!(custody.frame(frame(1, 4, Mode::Insert)));
    }

    #[test]
    fn entry_intent_keeps_the_first_press_across_stale_normal_repeats() {
        let mut custody = inserting();
        custody.key(frame(1, 2, Mode::Insert), Key::Escape);
        custody.frame(frame(1, 3, Mode::Normal));
        let first = Instant::now();
        custody.key_at(frame(1, 3, Mode::Normal), Key::Entry('i'), first);
        custody.key_at(
            frame(1, 3, Mode::Normal),
            Key::Entry('i'),
            first + std::time::Duration::from_millis(1),
        );
        custody.frame(frame(1, 4, Mode::Unknown));
        custody.key(frame(1, 4, Mode::Unknown), Key::Entry('a'));
        assert_eq!(custody.entry, Some((first, 'i')));
        assert!(custody.frame(frame(1, 5, Mode::Insert)));
        custody.key(frame(1, 5, Mode::Insert), Key::Boundary);
        assert_eq!(custody.entry, None);
        custody.frame(frame(2, 6, Mode::Insert));
        assert_eq!(custody.entry, None);
    }

    #[test]
    fn failed_entry_revokes_only_its_own_intent() {
        let custody = Custody::default();
        let at = Instant::now();
        custody.state.lock().unwrap().entry = Some((at, 'i'));
        custody.reject_entry(at + std::time::Duration::from_millis(1));
        assert_eq!(custody.state.lock().unwrap().entry, Some((at, 'i')));
        custody.reject_entry(at);
        assert_eq!(custody.state.lock().unwrap().entry, None);
        assert_eq!(custody.epoch(), 1);

        let f = |generation: u64, mode: Mode, epoch: u64| Frame {
            epoch,
            ..frame(1, generation, mode)
        };
        custody.key(f(1, Mode::Insert, custody.epoch()), Key::Text);
        assert!(custody.frame(f(2, Mode::Insert, custody.epoch())));
        custody.key(f(2, Mode::Insert, custody.epoch()), Key::Escape);
        assert!(!custody.frame(f(3, Mode::Normal, custody.epoch())));
        custody.key_at(f(3, Mode::Normal, custody.epoch()), Key::Entry('i'), at);
        custody.key(f(3, Mode::Normal, custody.epoch()), Key::Boundary);
        custody.reject_entry(at + std::time::Duration::from_millis(1));
        assert!(custody.state.lock().unwrap().hint_editable);
        custody.reject_entry(at);
        assert!(!custody.state.lock().unwrap().hint_editable);
        assert!(!custody.frame(f(4, Mode::ExitHint, custody.epoch())));
        assert!(!custody.key(f(4, Mode::ExitHint, custody.epoch()), Key::Text));
    }

    #[test]
    fn intermediate_normal_or_unknown_repaints_do_not_invent_an_exit_boundary() {
        let mut custody = inserting();
        custody.key(frame(1, 2, Mode::Insert), Key::Escape);
        custody.frame(frame(1, 3, Mode::Normal));
        custody.key(frame(1, 3, Mode::Normal), Key::Text);
        assert!(!custody.frame(frame(1, 4, Mode::Normal)));
        custody.key(frame(1, 4, Mode::Normal), Key::Text);
        assert!(!custody.frame(frame(1, 5, Mode::Unknown)));
        custody.key(frame(1, 5, Mode::Unknown), Key::Text);
        assert!(custody.frame(frame(1, 6, Mode::Insert)));
    }

    #[test]
    fn old_insert_and_new_streaming_repaint_do_not_license_normal_motion() {
        let mut custody = inserting();
        assert!(!custody.key(frame(1, 2, Mode::Insert), Key::Escape));
        assert!(!custody.key(frame(1, 2, Mode::Insert), Key::Text));
        assert!(!custody.frame(frame(1, 3, Mode::Insert)));
        assert!(!custody.key(frame(1, 3, Mode::Insert), Key::Text));
        assert!(!custody.frame(frame(1, 4, Mode::Unknown)));
        assert!(!custody.frame(frame(1, 5, Mode::Insert)));
    }

    #[test]
    fn confirmed_mode_is_separate_from_the_engines_echo_proof() {
        let mut custody = inserting();
        custody.key(frame(1, 2, Mode::Insert), Key::Escape);
        custody.frame(frame(1, 3, Mode::Normal));
        custody.key(frame(1, 3, Mode::Normal), Key::Text);
        assert!(custody.frame(frame(1, 4, Mode::Insert)));
        assert!(custody.key(frame(1, 4, Mode::Insert), Key::Text));
    }

    #[test]
    fn ctrl_c_hint_preserves_known_insert_but_never_settles_an_escape() {
        for boundary in [Key::Boundary, Key::Escape] {
            let mut custody = inserting();
            custody.key(frame(1, 2, Mode::Insert), boundary);
            assert_eq!(
                custody.frame(frame(1, 3, Mode::ExitHint)),
                matches!(boundary, Key::Boundary)
            );
            let allowed = custody.key(frame(1, 3, Mode::ExitHint), Key::Text);
            assert_eq!(allowed, matches!(boundary, Key::Boundary));
        }
    }

    #[test]
    fn ctrl_c_after_an_entry_the_agent_has_not_drawn_keeps_the_hint_editable() {
        for (entry, editable) in [(Some('i'), true), (None, false)] {
            let mut custody = inserting();
            custody.key(frame(1, 2, Mode::Insert), Key::Escape);
            custody.frame(frame(1, 3, Mode::Normal));
            if let Some(glyph) = entry {
                custody.key(frame(1, 3, Mode::Normal), Key::Entry(glyph));
            }
            custody.key(frame(1, 3, Mode::Normal), Key::Boundary);
            assert_eq!(
                custody.frame(frame(1, 4, Mode::ExitHint)),
                editable,
                "{entry:?}, then Ctrl-C before the repaint: the hint frame"
            );
            assert_eq!(
                custody.key(frame(1, 4, Mode::ExitHint), Key::Text),
                editable
            );
        }
    }

    #[test]
    fn pending_text_does_not_cross_terminal_owners() {
        let mut custody = inserting();
        custody.key(frame(1, 2, Mode::Insert), Key::Text);
        assert!(!custody.frame(frame(2, 3, Mode::Insert)));
        assert!(!custody.frame(frame(1, 4, Mode::Insert)));
    }

    #[test]
    fn contended_event_is_not_lost_as_permission_to_keep_old_insert_evidence() {
        let custody = Custody::default();
        custody.key(frame(1, 1, Mode::Insert), Key::Text);
        let held = custody.state.lock().unwrap();
        assert!(!custody.key(frame(1, 2, Mode::Insert), Key::Escape));
        drop(held);
        assert!(!custody.key(frame(1, 3, Mode::Insert), Key::Text));
        let mut current = frame(1, 4, Mode::Insert);
        current.epoch = custody.epoch();
        assert!(!custody.frame(current));
    }

    #[test]
    fn mode_requires_a_complete_composer_and_distinguishes_exit_hint() {
        let rule = "─".repeat(30);
        for (footer, expected) in [
            ("-- INSERT -- ⏵⏵ auto mode on", Mode::Insert),
            ("⏵⏵ auto mode on", Mode::Normal),
            ("Press Ctrl-C again to exit", Mode::ExitHint),
            ("", Mode::Unknown),
            ("some app text", Mode::Unknown),
        ] {
            let rows = vec![rule.clone(), "❯ prompt".into(), rule.clone(), footer.into()];
            assert_eq!(mode(&rows, 1), expected);
            assert_eq!(mode(&rows, 3), Mode::Unknown);
        }
    }
}
