// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The echo proof (ATERM-KI-1025-1): a recording keeps a typed key a program reads raw only once
//! the live screen shows that key's own character, visibly, where it was typed.
//!
//! A key aterm cannot see echoed when it is pressed (a program reads it raw with echo off, or the
//! tty's mode is unreadable, and no prompt of aterm's own shell is open for it) is held: the
//! recording logs nothing until a frame proves the key's echo ([`echo_verdict`]), and then logs
//! the key at that frame's instant, never at its press. Every other outcome stops the recording
//! before anything more is logged, as a no-echo key does, and taints the glow so the next attach
//! starts it from rest.
//!
//! The held characters live in memory only, for at most [`ECHO_PROOF_BOUND`]: no `Debug`, log,
//! trace or `aos` reply carries one.

use std::time::{Duration, Instant};

use aterm_core::terminal::{RenderCell, Terminal};
use aterm_effects::aos_momentum::RAW_KEY;
use aterm_effects::cursor_momentum::MOMENTUM_GLOW_TAU_S;
use aterm_session::sink::InputEpoch;

use super::AosWindow;

/// How long a held key waits for its program's own echo before the recording stops for it.
pub(crate) const ECHO_PROOF_BOUND: Duration = Duration::from_secs(1);

/// The most columns of typed keys a hold keeps waiting, and of a proven run it keeps checking.
pub(crate) const HOLD_CAP: usize = 32;

/// The glyphs password prompts mask typed keys with: one where a held key's echo should be
/// refutes the hold at once, and none counts toward corroborating a run.
pub(crate) const MASK_GLYPHS: [char; 10] = ['*', '•', '●', '◦', '∙', '·', '∗', '⁎', '✱', '○'];

#[cfg(test)]
thread_local! {
    /// Whether a test admits every raw key as a recorded one, as the code before the hold did: the
    /// negative control's switch.
    pub(crate) static ECHO_HOLD_OFF: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Whether a test releases a held run on a frame that proves nothing: the claim-2 binding's
    /// mutant.
    pub(crate) static RELEASE_ON_PENDING: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// One held key.
#[derive(Clone, Copy)]
struct EchoKey {
    /// One past its last cell, counted from the hold's start.
    end: u16,
    /// The terminal's print sequence at its press: its echo is a print after it.
    seq: u64,
    /// Its press.
    at: Instant,
    /// The session's input epoch once its bytes were written: a run continues only where no other
    /// input reached the program since.
    epoch: InputEpoch,
    /// Whether no other input reached the session between the key before it and its press (for a
    /// hold's first key, whether it continues the last proven run): a key is released on the run's
    /// corroboration only when it and every key before it back to the run do.
    joins: bool,
}

/// Keys typed into a recording that wait for their program's own echo, oldest first.
pub(crate) struct EchoHold {
    pub(super) session: u64,
    /// The screen row and column the first held key was typed at, as the press saw the cursor.
    row: u16,
    start: u16,
    /// The press's coordinate generation and screen: a scroll, re-grid or screen swap refutes.
    coord_gen: u64,
    alt: bool,
    /// The cells the held keys should show, per column: the lead char, `'\0'` at a wide
    /// continuation.
    cells: Vec<char>,
    keys: Vec<EchoKey>,
    /// The proven run's last cells, left of `start`, which must still show as typed.
    run: Vec<char>,
}

impl std::fmt::Debug for EchoHold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EchoHold")
            .field("session", &self.session)
            .field("keys", &self.keys.len())
            .finish_non_exhaustive()
    }
}

/// Where the last proven run ended, for a hold that opens exactly there with no other input
/// between.
pub(crate) struct EchoRun {
    session: u64,
    row: u16,
    end: u16,
    coord_gen: u64,
    alt: bool,
    epoch: InputEpoch,
    tail: Vec<char>,
}

impl std::fmt::Debug for EchoRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EchoRun")
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

/// What a frame reads of the hold's session: its live screen (never the viewport, aterm's
/// predicted echo or an IME preedit, which are drawn only into the frame's own copy).
#[derive(Default)]
pub(crate) struct EchoView {
    session: u64,
    cursor: (u16, u16),
    pending_wrap: bool,
    coord_gen: u64,
    alt: bool,
    display_offset: usize,
    print_seq: u64,
    sync_unfinished: bool,
    /// The hold's row, per column ([`Terminal::screen_row_echo_cells_into`]).
    chars: Vec<char>,
    visible: Vec<bool>,
    drawn: Vec<RenderCell>,
}

impl std::fmt::Debug for EchoView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EchoView")
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

impl EchoView {
    /// Reads `t`, the live terminal of `session`, for `hold`.
    pub(crate) fn read(&mut self, t: &Terminal, session: u64, hold: &EchoHold) {
        let c = t.cursor();
        self.session = session;
        self.cursor = (c.row, c.col);
        self.pending_wrap = t.grid().pending_wrap();
        self.coord_gen = t.content_scroll_state().coordinate_generation();
        self.alt = t.is_alternate_screen();
        self.display_offset = t.grid().display_offset();
        self.print_seq = t.print_seq();
        self.sync_unfinished = t.sync_frame_unfinished();
        t.screen_row_echo_cells_into(
            hold.row,
            &mut self.chars,
            &mut self.visible,
            &mut self.drawn,
        );
    }
}

/// What a frame proves of a hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Echo {
    /// The oldest `n` keys are shown echoed: log them now.
    Release(usize),
    /// Nothing proven yet, and the bound has not passed.
    Pending,
    /// The keys cannot be proven: stop the recording.
    Refute,
}

/// What a typed key's press saw, under the press's terminal lock.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Press {
    pub(crate) session: u64,
    pub(crate) cursor: (u16, u16),
    pub(crate) pending_wrap: bool,
    pub(crate) coord_gen: u64,
    pub(crate) alt: bool,
    pub(crate) display_offset: usize,
    pub(crate) print_seq: u64,
    /// The session's input epoch before the key's bytes are written.
    pub(crate) epoch: InputEpoch,
    pub(crate) at: Instant,
    /// What the live screen showed where the key's echo would land.
    pub(crate) row: PressRow,
}

/// What the live screen showed, at a press, where the key's echo would land: a column that already
/// shows the key's glyph would show it whether or not the program echoed the key, so
/// [`admit_key`] refuses a key typed over its own glyph.
#[derive(Clone, Copy)]
pub(crate) struct PressRow {
    /// The row and first column read, or `None` when the press read nothing.
    at: Option<(u16, u16)>,
    /// [`HOLD_CAP`] columns from `at`, in [`Terminal::row_cols_into`]'s convention.
    chars: [char; HOLD_CAP],
}

impl std::fmt::Debug for PressRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PressRow")
            .field("at", &self.at)
            .finish_non_exhaustive()
    }
}

impl PressRow {
    /// Nothing read: a press with no recording to hold a key for.
    pub(crate) const NONE: Self = Self {
        at: None,
        chars: [' '; HOLD_CAP],
    };

    /// Reads `t`'s live screen (never the viewport) for [`HOLD_CAP`] columns from `site`, the open
    /// hold's next column ([`next_site`]), or from the cursor when no hold is open.
    pub(crate) fn read(t: &Terminal, site: Option<(u16, u16)>) -> Self {
        let c = t.cursor();
        let (row, col) = site.unwrap_or((c.row, c.col));
        let view = t.grid().screen_row_view(row);
        let mut chars = [' '; HOLD_CAP];
        for (slot, i) in chars.iter_mut().zip(0u16..) {
            *slot = view.col_char(col.saturating_add(i));
        }
        Self {
            at: Some((row, col)),
            chars,
        }
    }

    /// Whether a column of `typed`, laid out from the row's first column read, already shows its
    /// glyph (a blank or a wide continuation proves nothing either way).
    fn shows_any(&self, typed: &[char]) -> bool {
        typed
            .iter()
            .zip(&self.chars)
            .any(|(&want, &got)| want != ' ' && want != '\0' && want == got)
    }
}

/// Where a key typed into `aos`'s open hold would show its echo (the hold's row, just past its
/// cells), or `None` with no hold open.
pub(crate) fn next_site(aos: &AosWindow) -> Option<(u16, u16)> {
    let h = aos.hold.as_ref()?;
    let len = u16::try_from(h.cells.len()).ok()?;
    Some((h.row, h.start.saturating_add(len)))
}

/// Where a typed key's instant goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyRoute {
    /// Logged at its press: the tty or an open prompt of aterm's shell echoes it.
    Record,
    /// Held for its echo ([`admit_key`] opened or extended the hold).
    Hold,
    /// The recording may not hold it ([`super::private_key`]).
    Private(&'static str),
}

/// Pushes the cells a typed key's echo shows, per column, onto `cells` (the lead char, then
/// `'\0'` for a wide glyph's second column), and returns whether it has any: a control char, a
/// zero-width or combining one, or text that holds one has none.
pub(crate) fn typed_echo_cells(
    typed: Option<char>,
    ime: Option<&str>,
    ambiguous_width_double: bool,
    cells: &mut Vec<char>,
) -> bool {
    let from = cells.len();
    let mut push = |ch: char| {
        let width = if ambiguous_width_double {
            aterm_grapheme::char_width_cjk(ch)
        } else {
            aterm_grapheme::char_width(ch)
        };
        if ch.is_control() || !(1..=2).contains(&width) {
            return false;
        }
        cells.push(ch);
        if width == 2 {
            cells.push('\0');
        }
        true
    };
    let all = match (ime, typed) {
        (Some(text), _) => !text.is_empty() && text.chars().all(&mut push),
        (None, Some(ch)) => push(ch),
        (None, None) => false,
    };
    if !all {
        cells.truncate(from);
    }
    all
}

/// Routes a typed key whose privacy verdict is `verdict` ([`super::key_privacy`]): a no-echo key
/// or Secure Keyboard Entry is private; a key aterm sees echoed is recorded at its press unless
/// keys are held, which it then joins; a raw key is held while the recording runs, the press was
/// on the live screen (no deferred wrap, no scrolled-back viewport), it has cells, none of the
/// columns its echo would take already shows its glyph ([`PressRow`]), and it fits the open hold
/// (its session, coordinates and screen, at most [`HOLD_CAP`] columns). Opening a hold asks the
/// host to hold the recording until the key's bound.
pub(crate) fn admit_key(
    aos: &mut AosWindow,
    momentum: &mut aterm_effects::aos_momentum::MomentumHost,
    verdict: Option<&'static str>,
    press: &Press,
    cells: impl FnOnce(&mut Vec<char>) -> bool,
) -> KeyRoute {
    match verdict {
        Some(why) if why != RAW_KEY => return KeyRoute::Private(why),
        None if aos.hold.is_none() => return KeyRoute::Record,
        _ => {}
    }
    #[cfg(test)]
    if ECHO_HOLD_OFF.with(std::cell::Cell::get) {
        return KeyRoute::Record;
    }
    let refused = KeyRoute::Private(RAW_KEY);
    if !momentum.aos_attached()
        || momentum.aos_paused()
        || press.pending_wrap
        || press.display_offset != 0
    {
        return refused;
    }
    let mut typed = Vec::new();
    if !cells(&mut typed) {
        return refused;
    }
    let site = if aos.hold.is_some() {
        next_site(aos)
    } else {
        Some(press.cursor)
    };
    if site.is_none() || press.row.at != site || press.row.shows_any(&typed) {
        return refused;
    }
    let width = |n: usize| u16::try_from(n).ok();
    match aos.hold.as_mut() {
        Some(h) => {
            let total = h.cells.len() + typed.len();
            let Some(end) = width(total).filter(|_| total <= HOLD_CAP) else {
                return refused;
            };
            if h.session != press.session || h.coord_gen != press.coord_gen || h.alt != press.alt {
                return refused;
            }
            let joins = h.keys.last().is_some_and(|k| k.epoch == press.epoch);
            h.cells.extend_from_slice(&typed);
            h.keys.push(EchoKey {
                end,
                seq: press.print_seq,
                at: press.at,
                epoch: press.epoch,
                joins,
            });
        }
        None => {
            let Some(end) = width(typed.len()).filter(|_| typed.len() <= HOLD_CAP) else {
                return refused;
            };
            let (row, start) = press.cursor;
            let run = match aos.echo_run.take() {
                Some(r)
                    if r.session == press.session
                        && r.row == row
                        && r.end == start
                        && r.coord_gen == press.coord_gen
                        && r.alt == press.alt
                        && r.epoch == press.epoch =>
                {
                    r.tail
                }
                _ => Vec::new(),
            };
            aos.hold = Some(EchoHold {
                session: press.session,
                row,
                start,
                coord_gen: press.coord_gen,
                alt: press.alt,
                cells: typed,
                keys: vec![EchoKey {
                    end,
                    seq: press.print_seq,
                    at: press.at,
                    epoch: press.epoch,
                    joins: !run.is_empty(),
                }],
                run,
            });
            momentum.aos_hold(press.at + ECHO_PROOF_BOUND);
        }
    }
    KeyRoute::Hold
}

/// Notes the session's input epoch once the newest held key's bytes were written.
pub(crate) fn note_written(aos: &mut AosWindow, epoch: InputEpoch) {
    if let Some(key) = aos.hold.as_mut().and_then(|h| h.keys.last_mut()) {
        key.epoch = epoch;
    }
}

/// The instant a hold's first key's bound passes, if one is open.
pub(crate) fn hold_deadline(aos: &AosWindow) -> Option<Instant> {
    aos.hold
        .as_ref()
        .and_then(|h| h.keys.first())
        .map(|k| k.at + ECHO_PROOF_BOUND)
}

/// Whether `run` holds at least two distinct chars that are neither a wide continuation, a blank
/// nor a mask glyph.
fn two_distinct(run: &[char]) -> bool {
    let mut first = None;
    for &c in run {
        if c == '\0' || c == ' ' || MASK_GLYPHS.contains(&c) {
            continue;
        }
        match first {
            None => first = Some(c),
            Some(f) if f != c => return true,
            Some(_) => {}
        }
    }
    false
}

/// What `view` proves of `hold` at `now`.
///
/// It refutes when the view is another session's, or the coordinates or the screen moved since the
/// first press. It proves nothing while a synchronized frame is unfinished or the viewport is
/// scrolled back. Otherwise, with the cursor on the hold's row past its start, the oldest `n` keys
/// whose cells end at or before the cursor are released when all of these hold:
/// - the live row shows exactly their cells from the start, every one visible, and the proven
///   run's tail still shows just left of it;
/// - the terminal printed since the last of them was pressed (and no column of theirs showed its
///   glyph at its press: [`admit_key`] refuses such a key);
/// - the cells are corroborated: two distinct typed glyphs that are not mask glyphs, or keys that
///   each continue the proven run, no other input reaching the session between them, with a
///   glyph that is not one.
///
/// A mask glyph where one of those cells should be refutes at once; any other mismatch (a torn
/// redraw) waits. Past [`ECHO_PROOF_BOUND`] from the first key's press, what is not proven is
/// refuted; a proof that first shows after the bound still releases, at that frame's instant.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        reason = "a key's echo is its program's output, which aterm only reads: this verdict is what the model's Release and Refute guards see of it",
        machine = "AosKeyHold",
        action = "Echo"
    )
)]
pub(crate) fn echo_verdict(hold: &EchoHold, view: &EchoView, now: Instant) -> Echo {
    if view.session != hold.session || view.coord_gen != hold.coord_gen || view.alt != hold.alt {
        return Echo::Refute;
    }
    if !view.sync_unfinished && view.display_offset == 0 {
        let col = view.cursor.1.saturating_add(u16::from(view.pending_wrap));
        if view.cursor.0 == hold.row && col > hold.start {
            let span = col - hold.start;
            let n = hold.keys.iter().take_while(|k| k.end <= span).count();
            if let Some(last) = n.checked_sub(1).and_then(|i| hold.keys.get(i)) {
                let start = usize::from(hold.start);
                let end = usize::from(last.end);
                let cells = &hold.cells[..end];
                let shown = view.chars.get(start..start + end);
                let visible = view
                    .visible
                    .get(start..start + end)
                    .is_some_and(|v| v.iter().all(|&b| b));
                let tail = start
                    .checked_sub(hold.run.len())
                    .and_then(|from| {
                        Some((view.chars.get(from..start)?, view.visible.get(from..start)?))
                    })
                    .is_some_and(|(c, v)| c == hold.run.as_slice() && v.iter().all(|&b| b));
                let printed = shown == Some(cells) && visible && tail && view.print_seq > last.seq;
                let continues = !hold.run.is_empty()
                    && hold.keys[..n].iter().all(|k| k.joins)
                    && cells
                        .iter()
                        .any(|&c| c != '\0' && !MASK_GLYPHS.contains(&c));
                if printed && (continues || two_distinct(cells)) {
                    return Echo::Release(n);
                }
                let masked = shown.is_some_and(|s| {
                    s.iter()
                        .zip(cells)
                        .any(|(&got, &want)| got != want && MASK_GLYPHS.contains(&got))
                });
                if masked {
                    return Echo::Refute;
                }
            }
        }
    }
    #[cfg(test)]
    if RELEASE_ON_PENDING.with(std::cell::Cell::get) {
        return Echo::Release(hold.keys.len());
    }
    match hold.keys.first() {
        Some(first) if now.saturating_duration_since(first.at) < ECHO_PROOF_BOUND => Echo::Pending,
        _ => Echo::Refute,
    }
}

/// Settles `ws`'s hold on the frame at `now` that read `view` (`None` when the frame could not
/// read the live screen: its lock busy, the pane rewound): a hold the host dropped by itself hands
/// its keys to the glow unrecorded; a proven run is logged at `now`; a refuted one stops the
/// recording. With no view nothing is settled: [`crate::App::aos_watch_via`] refutes once the bound and
/// a frame have passed.
pub(crate) fn settle_hold(ws: &mut crate::WindowState, view: Option<&EchoView>, now: Instant) {
    let Some(hold) = ws.aos.hold.as_ref() else {
        return;
    };
    if !ws.fx.cursor().momentum.aos_holding() {
        let dropped = ws.aos.hold.take();
        hand_over(ws, dropped, now);
        return;
    }
    let Some(view) = view else {
        return;
    };
    match echo_verdict(hold, view, now) {
        Echo::Release(n) => release(ws, n, now),
        Echo::Refute => super::private_key(ws, RAW_KEY, now),
        Echo::Pending => {}
    }
}

/// Logs the hold's oldest `n` keys at `now` and keeps the rest waiting until their own bound.
fn release(ws: &mut crate::WindowState, n: usize, now: Instant) {
    let Some(hold) = ws.aos.hold.as_mut() else {
        return;
    };
    let n = n.min(hold.keys.len());
    let Some(last) = n.checked_sub(1).and_then(|i| hold.keys.get(i)).copied() else {
        return;
    };
    let end = usize::from(last.end);
    hold.run.extend(hold.cells.drain(..end));
    let over = hold.run.len().saturating_sub(HOLD_CAP);
    hold.run.drain(..over);
    hold.keys.drain(..n);
    for k in &mut hold.keys {
        k.end -= last.end;
    }
    hold.start = hold.start.saturating_add(last.end);
    let rest = hold.keys.first().map(|k| k.at + ECHO_PROOF_BOUND);
    let keys = u32::try_from(n).unwrap_or(u32::MAX);
    ws.fx
        .cursor_mut()
        .momentum
        .aos_release(now, keys, MOMENTUM_GLOW_TAU_S, rest);
    if rest.is_none()
        && let Some(done) = ws.aos.hold.take()
    {
        ws.aos.echo_run = Some(EchoRun {
            session: done.session,
            row: done.row,
            end: done.start,
            coord_gen: done.coord_gen,
            alt: done.alt,
            epoch: last.epoch,
            tail: done.run,
        });
    }
}

/// `hold`'s keys to the glow at `now`, unrecorded (the recording is gone or never held them).
/// The last proven run ends with it, held keys or none: a run never continues across a detach.
pub(crate) fn hand_over(ws: &mut crate::WindowState, hold: Option<EchoHold>, now: Instant) {
    ws.aos.echo_run = None;
    let Some(hold) = hold else {
        return;
    };
    let momentum = &mut ws.fx.cursor_mut().momentum;
    momentum.taint();
    for _ in &hold.keys {
        momentum.on_key(now, MOMENTUM_GLOW_TAU_S);
    }
}

/// How many typed keys `aos`'s hold keeps waiting.
#[cfg(test)]
pub(crate) fn held_keys(aos: &AosWindow) -> usize {
    aos.hold.as_ref().map_or(0, |h| h.keys.len())
}

#[cfg(test)]
#[path = "aos_echo_tests.rs"]
mod tests;
