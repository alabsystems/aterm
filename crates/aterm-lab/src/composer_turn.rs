// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LAB TYPES A TURN ONLY INTO A COMPOSER THAT STANDS READY (unit J0-4, gap E6-g5): the one
//! turn routine behind `lab turn` on the lab's own tab ([`crate::verbs::turn`]) and a prompt typed
//! into a tab `@sid` ([`crate::forever::tab_turn`]).
//!
//! `lab claude` returns once the TUI draws anything (`❯|Welcome|trust|Claude Code`: its banner,
//! or a folder-trust dialog), and a prompt sent before the composer existed never stood in it: on
//! Claude Code 2.1.312 the first `lab turn` right after `lab claude` failed so, and a second one
//! worked. [`turn`] therefore (a) waits for the agent's empty composer with
//! [`composer_wait`], the hang detector the lab's tabs already wait with, (b) only then sends the
//! text, and (c) presses Enter once the text stands in the composer, again if it still stands 5 s
//! on, and then waits for the composer to take it. Its look, send and key steps are [`Steps`], so a
//! test scripts them.

use std::time::{Duration, Instant};

use crate::door::{Door, Reply};
use crate::forever::{COMPOSER_HANG, ComposerWait, Stand, composer_wait};
use crate::{Fail, stop};

/// A HANG DETECTOR, not a latency budget, for the text to stand in the composer once sent, and
/// for the composer to take it after both Enters: Claude Code 2.1.316 can leave a fresh tab's input
/// unread for more than 10 s while it starts (`status input=pending`), and, starting beside
/// fifteen more tabs, left a drafted first prompt standing for more than 10 s past both Enters
/// (slice 9c's labs), taking it once it read its input.
pub(crate) const DRAFT_HANG: Duration = Duration::from_secs(60);

/// How long the text may stand after the first Enter before Enter is pressed again: a second Enter
/// the program reads late lands on an empty composer.
const ENTER_AGAIN: Duration = Duration::from_secs(5);

/// The pause between two looks while waiting.
const LOOK_EVERY: Duration = Duration::from_millis(100);

/// The pause between two looks after the first Enter.
const ENTER_LOOK: Duration = Duration::from_millis(250);

/// What one look at a turn's target read: the screen as `ctl text` gave it and what stands on it
/// ([`stand_of`]); `None` where the screen could not be read.
pub(crate) type Look = Option<(String, Stand)>;

/// The steps a turn takes on its target, injected so a test can script them.
pub(crate) trait Steps {
    /// One look at the target's screen.
    fn look(&self) -> Look;
    /// Types `text` (`ctl send`): whether the host took it.
    /// # Errors
    /// The client could not run.
    fn send(&self, text: &str) -> Result<bool, Fail>;
    /// Presses Enter (`ctl key enter`): whether the host took it.
    /// # Errors
    /// The client could not run.
    fn enter(&self) -> Result<bool, Fail>;
    /// The time now.
    fn now(&self) -> Instant;
    /// Waits `d` and says the time.
    /// # Errors
    /// The run was interrupted.
    fn pause(&self, d: Duration) -> Result<Instant, stop::Stopped>;
}

/// How a turn ended ([`turn`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Turned {
    /// The composer took the text: it stood there, Enter was pressed, and it left.
    Taken,
    /// No empty composer stood, so nothing was sent: how the wait ended ([`ComposerWait`]: a
    /// draft or an editor no one typed, or a hang), and, for a hang, the last row of the screen it
    /// last read (its status row: a dialog's keys, a banner's line), never the whole screen.
    NotReady {
        wait: ComposerWait,
        last: Option<String>,
    },
    /// The host refused this step (`send` or `key enter`).
    Refused(&'static str),
    /// The text never stood alone in the composer within [`DRAFT_HANG`], so Enter was not
    /// pressed: the composer's row it last read (`None`: no composer).
    NotStood(Option<String>),
    /// The screen, or what its composer holds ([`Stand::Unknown`]), could not be read after Enter:
    /// the text is not known to be sent.
    Unread,
    /// The text still stood in the composer [`DRAFT_HANG`] past two Enters.
    StillStands,
}

impl Turned {
    /// Why `agent`'s turn of `text` failed (`None`: it did not).
    pub(crate) fn why(&self, agent: &str, text: &str) -> Option<String> {
        Some(match self {
            Self::Taken => return None,
            Self::NotReady { wait, last } => {
                let why = wait.why(agent).unwrap_or_default();
                match last {
                    Some(row) => format!("{why}, its last row {row:?}: '{text}' not typed"),
                    None => format!("{why}: '{text}' not typed"),
                }
            }
            Self::Refused(step) => format!("the host refused `{step}` for '{text}'"),
            Self::NotStood(row) => format!(
                "'{text}' did not stand in {agent}'s composer within {} s (it read {}): Enter not \
                 pressed",
                DRAFT_HANG.as_secs(),
                row.as_ref()
                    .map_or_else(|| "no composer".to_string(), |r| format!("{r:?}"))
            ),
            Self::Unread => format!(
                "{agent}'s composer could not be read after Enter: '{text}' is not known to be sent"
            ),
            Self::StillStands => format!(
                "'{text}' still stood in {agent}'s composer {} s after two Enters",
                DRAFT_HANG.as_secs()
            ),
        })
    }
}

/// What stands on `screen` for `agent` ([`Stand`]): its composer's first row
/// ([`crate::screen::composer_at`]), an editor holding the tab ([`crate::screen::editor_row`]), or
/// neither (still starting: a banner, a folder-trust dialog). `ctl text` cannot tell a placeholder
/// no one typed (Claude Code's grey suggestion, Codex's hint, Muse Code's prompt hint) from typed
/// text, so a composer that holds text is read again by its first glyph's cell, the harness's own
/// rule ([`aterm_agent::supervise::screen::composer_glyph_cell`],
/// [`aterm_agent::supervise::screen::cell_is_placeholder`] over the agent's reader's hint colour,
/// [`aterm_phase::ScreenReader::placeholder_fg`]): drawn dim, or in that hint colour (Muse Code's
/// `676c74`, no `dim` attribute), it stands empty; drawn otherwise, it holds that text; a cell
/// that says neither (`cell(row, col)`, that row's `ctl cell` reply, gave none, or an unfinished
/// frame's) is no evidence yet ([`Stand::Unknown`]).
pub(crate) fn stand_of(
    agent: &str,
    screen: &str,
    mut cell: impl FnMut(usize, usize) -> Option<String>,
) -> Stand {
    use aterm_agent::supervise::screen::{cell_is_placeholder, composer_glyph_cell};
    match crate::screen::composer_at(agent, screen) {
        Some((_, text)) if text.is_empty() => Stand::Composer(text),
        Some((row, text)) => {
            let hint = aterm_phase::program_of(agent).and_then(|p| p.reader().placeholder_fg());
            match composer_glyph_cell(|col| cell(row, col))
                .and_then(|c| cell_is_placeholder(&c, hint))
            {
                Some(true) => Stand::Composer(String::new()),
                Some(false) => Stand::Composer(text),
                None => Stand::Unknown,
            }
        }
        None => crate::screen::editor_row(screen).map_or(Stand::Starting, Stand::Editor),
    }
}

/// The last row of `screen` that is not blank, trimmed.
fn last_row(screen: &str) -> Option<String> {
    screen
        .lines()
        .map(str::trim)
        .rfind(|r| !r.is_empty())
        .map(str::to_string)
}

/// Looks every `every` until `done` holds for a look or `hang` passes (a hang detector, never a
/// budget); whether it held, and the last look.
fn look_until(
    steps: &impl Steps,
    hang: Duration,
    every: Duration,
    mut done: impl FnMut(&Look) -> bool,
) -> Result<(bool, Look), stop::Stopped> {
    let start = steps.now();
    let mut now = start;
    loop {
        let seen = steps.look();
        if done(&seen) {
            return Ok((true, seen));
        }
        if now.saturating_duration_since(start) >= hang {
            return Ok((false, seen));
        }
        now = steps.pause(every)?;
    }
}

/// Types `text` as the next turn of the agent on `steps`' target: (a) waits for its empty composer
/// ([`composer_wait`] over [`Steps::look`], ending at once on a draft or an editor no one typed,
/// or on a screen that stood unchanged [`COMPOSER_HANG`]), (b) sends the text, and (c) presses
/// Enter once the text stands alone in the composer, and again where it still stands
/// [`ENTER_AGAIN`] on; taken once a read screen no longer shows it there. A look that could not
/// read the screen, or what its composer holds ([`Stand::Unknown`]), is no evidence either way:
/// it is looked past, and a turn whose last look after Enter was one is [`Turned::Unread`].
///
/// # Errors
/// The client could not run, or the run was interrupted.
pub(crate) fn turn(steps: &impl Steps, text: &str) -> Result<Turned, Fail> {
    let mut last: Option<String> = None;
    let wait = composer_wait(
        || match steps.look() {
            Some((screen, stand)) => {
                last = Some(screen.clone());
                (Some(screen), stand)
            }
            None => (None, Stand::Starting),
        },
        COMPOSER_HANG,
        steps.now(),
        || steps.pause(LOOK_EVERY),
    )?;
    if wait != ComposerWait::Ready {
        let last = match wait {
            ComposerWait::Hung => last.as_deref().and_then(last_row),
            _ => None,
        };
        return Ok(Turned::NotReady { wait, last });
    }
    if !steps.send(text)? {
        return Ok(Turned::Refused("send"));
    }
    let stands = |seen: &Look| matches!(seen, Some((_, Stand::Composer(t))) if t == text);
    let read = |seen: &Look| matches!(seen, Some((_, stand)) if *stand != Stand::Unknown);
    let left = |seen: &Look| read(seen) && !stands(seen);
    let (stood, seen) = look_until(steps, DRAFT_HANG, LOOK_EVERY, stands)?;
    if !stood {
        return Ok(Turned::NotStood(match seen {
            Some((_, Stand::Composer(row))) => Some(row),
            _ => None,
        }));
    }
    for press in 0..2 {
        if !steps.enter()? {
            return Ok(Turned::Refused("key enter"));
        }
        if press == 0 && look_until(steps, ENTER_AGAIN, ENTER_LOOK, left)?.0 {
            return Ok(Turned::Taken);
        }
    }
    let (taken, seen) = look_until(steps, DRAFT_HANG, LOOK_EVERY, left)?;
    Ok(if taken {
        Turned::Taken
    } else if read(&seen) {
        Turned::StillStands
    } else {
        Turned::Unread
    })
}

/// A turn's target: the lab's own tab, or a tab `@sid` of it, running `agent`.
pub(crate) struct Target<'d, 'r> {
    door: &'d Door<'r>,
    at: Option<String>,
    agent: &'d str,
}

impl<'d, 'r> Target<'d, 'r> {
    /// The lab's own tab.
    pub(crate) fn lab(door: &'d Door<'r>, agent: &'d str) -> Self {
        Self {
            door,
            at: None,
            agent,
        }
    }

    /// Tab `sid` of the lab.
    pub(crate) fn tab(door: &'d Door<'r>, sid: &str, agent: &'d str) -> Self {
        Self {
            door,
            at: Some(format!("@{sid}")),
            agent,
        }
    }

    fn ctl(&self, args: &[&str]) -> Result<Reply, String> {
        let mut v: Vec<&str> = self.at.iter().map(String::as_str).collect();
        v.extend_from_slice(args);
        self.door.ctl(&v)
    }

    /// `args`' standard output where the host answered (exit 0).
    fn read(&self, args: &[&str]) -> Option<String> {
        self.ctl(args)
            .ok()
            .filter(|r| r.code == 0)
            .map(|r| r.stdout)
    }
}

impl Steps for Target<'_, '_> {
    fn look(&self) -> Look {
        let screen = self.read(&["text"])?;
        let stand = stand_of(self.agent, &screen, |row, col| {
            self.read(&["cell", &row.to_string(), &col.to_string()])
        });
        Some((screen, stand))
    }

    fn send(&self, text: &str) -> Result<bool, Fail> {
        Ok(self.ctl(&["send", text])?.code == 0)
    }

    fn enter(&self) -> Result<bool, Fail> {
        Ok(self.ctl(&["key", "enter"])?.code == 0)
    }

    fn now(&self) -> Instant {
        Instant::now()
    }

    fn pause(&self, d: Duration) -> Result<Instant, stop::Stopped> {
        stop::sleep(d)?;
        Ok(Instant::now())
    }
}

#[cfg(test)]
#[path = "composer_turn_tests.rs"]
mod tests;
