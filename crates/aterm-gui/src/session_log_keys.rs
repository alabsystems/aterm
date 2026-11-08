// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a key chose on the approval box or question it was pressed on (fix E8, finding E2E-8):
//! the key's class ([`KeyClass`]) and the box's options as the phase reader read them on the last
//! frame before the key ([`FrameKeys::of_prompt`], read by presence's adapter beside the verdict
//! it publishes). A key alone says nothing of what it meant (T1); the focused option, or the
//! option a digit numbers, and its role are the evidence, whoever pressed the key. What the phase
//! reader cannot place (typed text, a navigation key, an unknown role, a standing grant whose
//! words do not affirm, no focused option, a box whose every option chooses alike) chooses
//! nothing, and the step closes `passed` as before.
//!
//! THE FRAME A KEY LANDS ON ([`KeyWitness`]): the last agent read before the key, and only while
//! it shows the screen the key landed on: the program's answer to every input before the key,
//! and nothing the program drew since. An input voids the frame it landed on; a later read counts
//! again only once the screen moved after that input (the PTY reader's count of feeds that moved
//! it, [`crate::vitals_host::ScreenMoves`]), so a read taken between a key's write and the
//! program's redraw never stands for the frame after the key. A screen that moved after the last
//! read (a box the program closed or changed by itself, before the sweep read it again) voids it
//! too, until a read sees that move.

use std::sync::Arc;

use aterm_phase::prompt::{CancelEffect, Select};
use aterm_phase::{Opt, PromptKind, PromptV2, QuestionFocus, QuestionForm, Role};
use aterm_slog::Chose;

/// A key's class: the keys that choose on a box or question, and every other input. An event is
/// classed on the input side ([`KeyClass::of_event`], in `crate::human_input`), so the log takes a
/// class and its code names no input event (census `slog-inert`: a mark never becomes input).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyClass {
    /// Enter: the focused option.
    Enter,
    /// Esc: the box's or question's cancel.
    Esc,
    /// A digit `1`-`9`: the option it numbers, on a box whose options a digit selects.
    Digit(u8),
    /// Anything else: typed text, a paste, a navigation key, a key under a modifier, a release.
    Other,
}

impl KeyClass {
    /// The class of raw bytes written to the program: a lone CR is Enter, a lone ESC is Esc, a
    /// lone digit is that digit.
    pub(crate) fn of_bytes(b: &[u8]) -> KeyClass {
        match b {
            [0x0d] => KeyClass::Enter,
            [0x1b] => KeyClass::Esc,
            [d @ b'1'..=b'9'] => KeyClass::Digit(d - b'0'),
            _ => KeyClass::Other,
        }
    }

    /// The class of one character: a digit `1`-`9`, CR (Enter) or ESC (Esc); any other is Other.
    pub(crate) const fn of_char(c: char) -> KeyClass {
        match c {
            '1'..='9' => KeyClass::Digit(c as u8 - b'0'),
            '\r' => KeyClass::Enter,
            '\u{1b}' => KeyClass::Esc,
            _ => KeyClass::Other,
        }
    }
}

/// What each choosing key would choose on one frame's box or question ([`Self::of_prompt`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FrameKeys {
    enter: Option<Chose>,
    esc: Option<Chose>,
    /// Digit `d` at `digits[d - 1]`.
    digits: [Option<Chose>; 9],
}

impl FrameKeys {
    /// What a key of `class` chooses on this frame.
    pub(crate) const fn chose(&self, class: KeyClass) -> Option<Chose> {
        match class {
            KeyClass::Enter => self.enter,
            KeyClass::Esc => self.esc,
            KeyClass::Digit(d @ 1..=9) => self.digits[d as usize - 1],
            KeyClass::Digit(_) | KeyClass::Other => None,
        }
    }

    /// THE READING of one box or question, as the phase reader parsed it: Enter chooses the
    /// focused option, a digit the option it numbers where the box selects by digits, Esc the
    /// cancel the footer names. On an approval box an option's role, and for a standing grant its
    /// words, say what it chooses ([`option_chose`]), and only on a box whose options do not all
    /// choose alike: a box that offers one outcome (a lone option) records no choice. An option
    /// of no role this log names (a label the capture garbled, a usage limit's wait, an MCP
    /// server's dialog, whose every option the reader reads as kept) chooses nothing. On a
    /// question, a chosen answer (Enter on
    /// an option or on typed free text, Enter on the last step's button, a digit naming an
    /// option, the review's Submit) answers it, and the review's Cancel declines it.
    pub(crate) fn of_prompt(p: &PromptV2) -> FrameKeys {
        let esc = p
            .cancel
            .as_ref()
            .filter(|c| matches!(c.effect, CancelEffect::Reject | CancelEffect::Exit))
            .map(|_| Chose::Decline);
        let mut keys = FrameKeys {
            esc,
            ..FrameKeys::default()
        };
        if p.kind == PromptKind::Question {
            if let Some(q) = &p.question_dialog {
                keys.question(&q.form, q.focus());
            }
            return keys;
        }
        let mut chosen = p.options.iter().map(option_chose);
        let first = chosen.next();
        if chosen.all(|c| Some(c) == first) {
            return keys;
        }
        keys.enter = p.focused().and_then(option_chose);
        if p.select == Select::Digits {
            for o in &p.options {
                if let Some(n @ 1..=9) = o.n {
                    keys.digits[usize::from(n) - 1] = option_chose(o);
                }
            }
        }
        keys
    }

    fn question(&mut self, form: &QuestionForm, focus: QuestionFocus) {
        let mut digit = |n: u8, c: Chose| {
            if (1..=9).contains(&n) {
                self.digits[usize::from(n) - 1] = Some(c);
            }
        };
        let enter = match form {
            QuestionForm::Single {
                options, free_text, ..
            } => {
                for o in options {
                    digit(o.n, Chose::Answer);
                }
                match focus {
                    QuestionFocus::Option(_) => Some(Chose::Answer),
                    QuestionFocus::FreeText if !free_text.pristine => Some(Chose::Answer),
                    _ => None,
                }
            }
            QuestionForm::Preview { options, .. } => {
                for o in options {
                    digit(o.n, Chose::Answer);
                }
                matches!(focus, QuestionFocus::Option(_)).then_some(Chose::Answer)
            }
            QuestionForm::Multi { .. } => {
                matches!(focus, QuestionFocus::Button).then_some(Chose::Answer)
            }
            QuestionForm::Review { .. } => {
                digit(1, Chose::Answer);
                digit(2, Chose::Decline);
                match focus {
                    QuestionFocus::ReviewSubmit => Some(Chose::Answer),
                    QuestionFocus::ReviewCancel => Some(Chose::Decline),
                    _ => None,
                }
            }
            QuestionForm::Unknown { .. } => None,
        };
        self.enter = enter;
    }
}

/// What choosing option `o` of an approval box does, read as the harness reads a box's yes
/// (`aterm-agent`'s `supervise::policy::approval`): a one-shot grant (`Once`, a folder's
/// `Trust`) approves the call; a standing grant (`Session`, `Persist`, `ModeSwitch`:
/// [`Role::standing`]) approves it only when its words affirm ([`Opt::affirms`], the harness's
/// own test of a yes), because the reader gives a standing role by the words that keep an answer
/// and a refusal's words keep one too (`No, and block reads outside the working directories`,
/// `Don't ask again`, an MCP server's dialog); a `No` (`Deny`) or an exit declines it; anything
/// else (a standing option whose words do not affirm, a usage limit's wait, an option of no
/// known role) chooses nothing.
pub(crate) fn option_chose(o: &Opt) -> Option<Chose> {
    if o.role.standing() {
        return o.affirms().then_some(Chose::Approve);
    }
    match o.role {
        Role::Once | Role::Trust => Some(Chose::Approve),
        Role::Deny | Role::Exit => Some(Chose::Decline),
        Role::Session
        | Role::Persist
        | Role::ModeSwitch
        | Role::AutoResume
        | Role::Answer
        | Role::Other => None,
    }
}

/// What the log knows of the frame the next key lands on ([`SessionLog`](crate::session_log::SessionLog)'s
/// half of fix E8): the last agent read's [`FrameKeys`], the screen moves it was read at, and the
/// inputs that reached the program since.
#[derive(Debug, Default)]
pub(crate) struct KeyWitness {
    /// The last agent read's keys (`None`: it showed no box or question) and the screen moves the
    /// read saw.
    frame: Option<FrameKeys>,
    read_moves: u64,
    /// The inputs that reached the program, a person's and control's alike.
    inputs: u64,
    /// The screen moves counted when the last of them reached it.
    input_moves: Option<u64>,
    /// The PTY reader's count of feeds that moved the screen, bound when the store registers the
    /// session; a log no store bound (a unit test's) counts the last read's moves instead.
    moves: Option<Arc<crate::vitals_host::ScreenMoves>>,
}

impl KeyWitness {
    /// Bind the session's count of screen moves.
    pub(crate) fn bind(&mut self, moves: Arc<crate::vitals_host::ScreenMoves>) {
        self.moves = Some(moves);
    }

    /// An agent read of the screen at `moves` (the count the reader saw under the term lock)
    /// showed `keys` (`None`: no box or question).
    pub(crate) const fn read(&mut self, keys: Option<FrameKeys>, moves: u64) {
        self.frame = keys;
        self.read_moves = moves;
    }

    /// An agent read of the screen at `moves` found the zone the last read classified unchanged
    /// (the sweep classifies no unchanged zone): the frame it showed stands at `moves`.
    pub(crate) fn reread(&mut self, moves: u64) {
        self.read_moves = self.read_moves.max(moves);
    }

    /// An input reached the program now: the frame it landed on stands for no later key.
    pub(crate) fn reached(&mut self) {
        self.inputs += 1;
        let now = self.moves.as_ref().map_or(self.read_moves, |m| m.count());
        self.input_moves = Some(now);
    }

    /// The frame a key pressed now lands on: the last read, when the screen it saw moved after
    /// the last input reached the program, and has not moved since (a log no store bound cannot
    /// tell, and takes the read as current).
    pub(crate) fn keys(&self) -> Option<FrameKeys> {
        let current = self
            .moves
            .as_ref()
            .is_none_or(|m| m.count() <= self.read_moves);
        self.frame
            .filter(|_| current && self.input_moves.is_none_or(|i| self.read_moves > i))
    }

    /// What a key of `class` pressed now chooses.
    pub(crate) fn chose(&self, class: KeyClass) -> Option<Chose> {
        self.keys().and_then(|k| k.chose(class))
    }

    /// The frame a control input admitted now lands on ([`KeyFrame::chose`] at its acceptance).
    pub(crate) fn at_admission(&self) -> KeyFrame {
        KeyFrame {
            keys: self.keys(),
            inputs: self.inputs,
        }
    }
}

/// The frame a control input landed on, read at its admission ([`KeyWitness::at_admission`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct KeyFrame {
    keys: Option<FrameKeys>,
    inputs: u64,
}

impl KeyFrame {
    /// What a key of `class` admitted on this frame chose, accepted with `now` as the log's
    /// witness: nothing when another input reached the program between its admission and its
    /// acceptance (the frame no longer showed the program's state before it).
    pub(crate) fn chose(self, class: KeyClass, now: &KeyWitness) -> Option<Chose> {
        self.keys
            .filter(|_| self.inputs == now.inputs)
            .and_then(|k| k.chose(class))
    }
}

#[cfg(test)]
#[path = "session_log_keys_tests.rs"]
pub(crate) mod tests;
