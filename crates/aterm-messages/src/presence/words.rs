// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The row's WORDS: the six slots composed from a [`Slot`] ([`words`]), laid
//! out at a width and shed from the right in the design's order
//! ([`Words::fit`], [`Words::pieces`]), the vocabulary every free-text
//! value is held to ([`sanitize_token`]), and the CELLS the host's grid lays
//! a string out in, which the width law, the token cap and the painter all
//! measure by.

use std::fmt::Write as _;
use std::marker::PhantomData;

use super::{AgentPhase, CTX_WARN_PCT, Hand, Host, Level, Link, Slot, StoryVerb, Tone, WallAnswer};
use crate::text::{is_stripped, truncate};
use crate::{Duration, Instant};

/// The six slots, composed. Each is a whole string; [`Words::fit`] lays them
/// out at a width and sheds from the right in the design's order.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Words {
    /// The role slot (`meta role`, or `—`).
    pub role: String,
    /// The phase slot's word.
    pub phase: String,
    /// The `since` clauses, in the order they are printed; elided shortest
    /// first when the row is narrow.
    pub since: Vec<String>,
    /// The hand slot.
    pub hand: String,
    /// The hand without its detail (`◂ turn 41` for `◂ manager · turn 41`,
    /// `⊘ hold` for `⊘ hold <reason>`): what survives past the ctx slot on a
    /// narrow row, so the hand is never cut mid-word.
    pub hand_short: String,
    /// The mail slot — empty, and no slot on the row, with no mail to tell
    /// (ruling 366).
    pub mail: String,
    /// The mail counts alone (`✉2 ↑1`), without `kind←✓from`.
    pub mail_short: String,
    /// The context slot (empty without a reading).
    pub ctx: String,
    /// The fabric slot.
    pub fabric: String,
    /// The a11y sentence.
    pub sentence: String,
    /// The tone the phase slot is painted in.
    pub tone: Tone,
}

/// The joint between since-clauses.
const CLAUSE_SEP: &str = " \u{00b7} ";
/// The empty-slot mark.
const DASH: &str = "\u{2014}";

/// Which slot a laid-out piece belongs to, so the painter can colour it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotKind {
    /// The role.
    Role,
    /// The phase word.
    Phase,
    /// The since-clauses.
    Since,
    /// The hand.
    Hand,
    /// The mail.
    Mail,
    /// The context.
    Ctx,
    /// The fabric link.
    Fabric,
}

impl Words {
    /// Lay the slots out at `cols` cells of the host's grid
    /// ([`Host::columns`]). Elision right→left: the fabric slot (`rtt`), then
    /// since-clauses shortest first (the longest stop survives), then the
    /// role, then ctx; then the mail's `kind←from` and the hand's detail fold
    /// to their short forms, so hand, phase and mail survive to 24 columns —
    /// past which the line is cut, between two cells.
    #[must_use]
    pub fn fit<V: Host>(&self, cols: usize) -> String {
        let pieces = self.pieces::<V>(cols);
        join_pieces(&pieces)
    }

    /// The pieces [`Self::fit`] keeps at `cols`, in order, each tagged with
    /// its slot: a `Since` piece follows its `Phase` by one space, every other
    /// piece follows its predecessor by two.
    #[must_use]
    pub fn pieces<V: Host>(&self, cols: usize) -> Vec<(SlotKind, String)> {
        let mut since = self.since.clone();
        let mut fabric = true;
        let mut role = true;
        let mut ctx = true;
        let mut mail_full = true;
        let mut hand_full = true;
        loop {
            let pieces = self.compose(&since, role, ctx, fabric, hand_full, mail_full);
            if line_width::<V>(&pieces) <= cols {
                return pieces;
            }
            if fabric {
                fabric = false;
                continue;
            }
            // The since-clauses, shortest first — but the stop clause (the
            // longest stop) outlives everything below and goes last of all.
            let is_stop = |c: &String| {
                c.starts_with("limited") || c.starts_with("held") || c.starts_with('\u{2192}')
            };
            if let Some(i) = since
                .iter()
                .enumerate()
                .filter(|(_, c)| !is_stop(c))
                .min_by_key(|(_, c)| width::<V>(c))
                .map(|(i, _)| i)
            {
                since.remove(i);
                continue;
            }
            if role {
                role = false;
                continue;
            }
            if ctx {
                ctx = false;
                continue;
            }
            if mail_full && self.mail_short != self.mail {
                mail_full = false;
                continue;
            }
            if hand_full && self.hand_short != self.hand {
                hand_full = false;
                continue;
            }
            if !since.is_empty() {
                since.clear();
                continue;
            }
            // Nothing left to shed: cut the survivors at the width — the
            // MAIL keeps its cells (it is the rightmost survivor and the
            // shortest), the hand gives way first, then the phase, so all
            // three are on the row down to 13 columns.
            if let [
                (SlotKind::Phase, phase),
                (SlotKind::Hand, hand),
                (SlotKind::Mail, mail),
            ] = pieces.as_slice()
            {
                let (pw, hw, mw) = (width::<V>(phase), width::<V>(hand), width::<V>(mail));
                if pw + hw + mw + 4 > cols && cols >= mw + 4 + 3 + 4 {
                    let hand_room = (cols - mw - 4).saturating_sub(pw).max(3);
                    let hand_cut = truncate_cells::<V>(hand, hand_room.min(hw));
                    let phase_room = cols - mw - 4 - width::<V>(&hand_cut);
                    let phase_cut = truncate_cells::<V>(phase, phase_room.min(pw));
                    return vec![
                        (SlotKind::Phase, phase_cut),
                        (SlotKind::Hand, hand_cut),
                        (SlotKind::Mail, mail.clone()),
                    ];
                }
            }
            let mut out = Vec::new();
            let mut used = 0usize;
            for (i, (kind, text)) in pieces.into_iter().enumerate() {
                let gap = gap_before(i, kind);
                if used + gap >= cols {
                    break;
                }
                let room = cols - used - gap;
                let cut = truncate_cells::<V>(&text, room);
                used += gap + width::<V>(&cut);
                out.push((kind, cut));
            }
            return out;
        }
    }

    #[allow(
        clippy::fn_params_excessive_bools,
        reason = "one switch per sheddable slot, in the shedding order `pieces` walks"
    )]
    fn compose(
        &self,
        since: &[String],
        role: bool,
        ctx: bool,
        fabric: bool,
        hand_full: bool,
        mail_full: bool,
    ) -> Vec<(SlotKind, String)> {
        let mut out = Vec::with_capacity(7);
        if role && !self.role.is_empty() {
            out.push((SlotKind::Role, self.role.clone()));
        }
        out.push((SlotKind::Phase, self.phase.clone()));
        if !since.is_empty() {
            out.push((SlotKind::Since, since.join(CLAUSE_SEP)));
        }
        out.push((
            SlotKind::Hand,
            if hand_full {
                &self.hand
            } else {
                &self.hand_short
            }
            .clone(),
        ));
        // No mail to tell is no slot (ruling 366): `✉0` read as jargon.
        if !self.mail.is_empty() {
            out.push((
                SlotKind::Mail,
                if mail_full {
                    &self.mail
                } else {
                    &self.mail_short
                }
                .clone(),
            ));
        }
        if ctx && !self.ctx.is_empty() {
            out.push((SlotKind::Ctx, self.ctx.clone()));
        }
        if fabric && !self.fabric.is_empty() {
            out.push((SlotKind::Fabric, self.fabric.clone()));
        }
        out
    }
}

/// The blank cells before the `i`-th laid-out piece: none before the first,
/// one before a `Since` piece (it follows its phase), two between slots.
pub(super) fn gap_before(i: usize, kind: SlotKind) -> usize {
    match (i, kind) {
        (0, _) => 0,
        (_, SlotKind::Since) => 1,
        _ => 2,
    }
}

/// Join laid-out pieces into the printed line, each after its
/// [`gap_before`].
pub(super) fn join_pieces(pieces: &[(SlotKind, String)]) -> String {
    let mut out = String::new();
    for (i, (kind, text)) in pieces.iter().enumerate() {
        out.extend(std::iter::repeat_n(' ', gap_before(i, *kind)));
        out.push_str(text);
    }
    out
}

/// The cells laid-out pieces take on the row: each piece's own ([`width`])
/// and the gaps between them — measured piece by piece, as the painter lays
/// each one out, so the law and the painted row agree cell for cell.
fn line_width<V: Host>(pieces: &[(SlotKind, String)]) -> usize {
    pieces
        .iter()
        .enumerate()
        .map(|(i, (kind, text))| gap_before(i, *kind) + width::<V>(text))
        .sum()
}

/// One cell of a string as the host's grid draws it ([`cells`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Cell {
    /// The byte the cell opens at: its own characters run from here to the
    /// next cell's `at` (or the string's end).
    pub(super) at: usize,
    /// The character that opened it — what a one-character cell paints.
    pub(super) ch: char,
    /// The columns it takes: 1, or 2 for a wide glyph (or one VS16 widened).
    pub(super) width: usize,
}

/// The glyphs with an emoji form that the row pins to TEXT presentation, one
/// cell each — the fleet lock included: U+1F512 is emoji-presentation by
/// default, and a renderer would take its colour face two cells wide
/// otherwise. All are the row's own ([`BAND_GLYPHS`]): no free text carries
/// one. The measure ([`cells`]) and the painter's pin read this one list.
pub(super) fn text_presentation(c: char) -> bool {
    matches!(
        c,
        '\u{2713}' | '\u{2717}' | '\u{26a0}' | '\u{2709}' | '\u{2715}' | '\u{1f512}'
    )
}

/// The characters one grid cell holds: the one that opened it and at most 16
/// more (aterm-grid's `CellExtra::MAX_COMBINING`, aterm-grapheme's
/// `MAX_JOINED`). The grid drops the rest, and so does the row ([`CellWalk`]).
const CELL_CHARS: usize = 1 + 16;

/// What one character does to the row's cells ([`CellWalk::step`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CellStep {
    /// It opens a cell of its own, this many columns wide.
    Opens(usize),
    /// It is drawn in the cell before it, which it widens by this many
    /// columns: a combining mark 0, VS16 after `©` 1.
    Joins(usize),
    /// Nothing of it is kept: a zero-width character with no cell to join
    /// (the grid keeps nothing of it either), or one more character for a
    /// cell that already holds [`CELL_CHARS`].
    Dropped,
}

/// The host's grid walked a character at a time, cell by cell, each cell
/// held to what the grid's cell holds ([`CELL_CHARS`]) — the one walk the
/// measure ([`cells`]) and the token's own pass ([`sanitize_token`]) share.
///
/// Whether a character opens a cell or is drawn in the one before it turns
/// on that cell alone (its base, its width, its last stored character), so
/// each character is measured against the current cell's characters and
/// nothing before them; and since a cell holds at most [`CELL_CHARS`], each
/// measure is bounded, and a walk is linear in its string whatever it holds.
///
/// THE CELL BOUND (ruling 2026-10-11, round fifteen content-6 review): the
/// walk once measured each character against ALL of its cell's text, and a
/// cell's text had no bound, so `1` and three thousand combining marks cost
/// a quadratic walk, and a token capped in cells carried them all (a
/// hostile screen row's reset became a 6 KB token, its sanitize, fit and
/// paint tens of milliseconds on the window's thread). The grid keeps a
/// cell's first 16 joined characters; the row keeps the same and drops the
/// rest, so nothing the grid draws is lost and a token holds at most
/// [`CELL_CHARS`] characters a cell. A VS16 past a full cell still widens
/// the grid's cell; the row drops it with the rest, and a token never
/// carries it, so a token's own grid width is still the row's measure of it.
struct CellWalk<V> {
    /// The current cell's characters, at most [`CELL_CHARS`] — stale while
    /// [`Self::opener`] holds the cell's one character.
    cell: String,
    /// The character that opened the current cell, not yet copied into
    /// [`Self::cell`]: most cells hold nothing else, so it is copied only
    /// when a character is measured against it, and a row of letters and
    /// glyphs never touches the buffer.
    opener: Option<char>,
    /// How many characters the current cell holds (0 before the first).
    held: usize,
    /// `V::columns` of the current cell, once measured.
    cols: Option<usize>,
    host: PhantomData<V>,
}

impl<V: Host> CellWalk<V> {
    const fn new() -> Self {
        Self {
            cell: String::new(),
            opener: None,
            held: 0,
            cols: None,
            host: PhantomData,
        }
    }

    /// Lay out `c` after everything kept so far. Printable ASCII opens a cell
    /// of its own wherever it stands (the grid's bulk path never folds it),
    /// and the glyphs pinned to [`text_presentation`] take one cell each;
    /// a printing character opens its own after no cell or after a cell of
    /// one ASCII character (the grid folds a character only into a cell
    /// holding a ZWJ, a regional indicator or a wide emoji base); everything
    /// else is measured.
    fn step(&mut self, c: char) -> CellStep {
        if c.is_ascii() || text_presentation(c) {
            self.open(c, None);
            return CellStep::Opens(1);
        }
        let own = V::columns(c.encode_utf8(&mut [0; 4]));
        let full = self.held >= CELL_CHARS;
        // A zero-width character joins the cell before it: with none, or a
        // full one, nothing of it is kept, and it can open nothing.
        if own == 0 && (self.held == 0 || full) {
            return CellStep::Dropped;
        }
        if own > 0 && (self.held == 0 || self.opener.is_some_and(|o| o.is_ascii())) {
            self.open(c, Some(own));
            return CellStep::Opens(own);
        }
        if let Some(opener) = self.opener.take() {
            self.cell.clear();
            self.cell.push(opener);
        }
        let before = match self.cols {
            Some(cols) => cols,
            None => V::columns(&self.cell),
        };
        self.cell.push(c);
        // What `c` adds to the cell it follows, in the grid's own walk.
        let grew = V::columns(&self.cell).saturating_sub(before);
        if own == 0 || grew == 0 {
            // Drawn in the cell before it: a zero-width character (which may
            // widen that cell), or one the grid folds into it.
            if full {
                self.cell.pop();
                return CellStep::Dropped;
            }
            self.held += 1;
            self.cols = Some(before + grew);
            return CellStep::Joins(grew);
        }
        self.open(c, Some(own));
        CellStep::Opens(grew)
    }

    /// Open a cell with `c`, `cols` wide once measured.
    fn open(&mut self, c: char, cols: Option<usize>) {
        self.opener = Some(c);
        self.held = 1;
        self.cols = cols;
    }
}

/// `s` laid out in the host's grid ([`Host::columns`]), cell by cell, as the
/// painter draws it ([`CellWalk`]): each character either OPENS a cell (a
/// wide glyph's two columns, any other's one) or is drawn in the cell before
/// it — a combining mark, a variation selector (VS16 widens the cell it
/// joins: `©️`), the rest of a ZWJ sequence or a flag — and belongs to that
/// cell; past a cell's [`CELL_CHARS`], or with no cell to join, it is
/// dropped. Linear in `s`.
///
/// THE WIDTH LAW (ruling 2026-10-11, round fifteen content-6): the row
/// counted one cell per `char`, so a CJK role was laid out in half the
/// columns its glyphs take (and painted squeezed, one per cell), and a
/// combining mark took a cell of its own. The row's cells are now the
/// grid's, for its free text and its own glyphs alike: for ASCII and the
/// row's glyphs (each one cell in the grid, the pinned lock one by the pin)
/// the law is unchanged.
pub(super) fn cells<V: Host>(s: &str) -> Cells<'_, V> {
    Cells {
        chars: s.char_indices(),
        walk: CellWalk::new(),
        open: None,
    }
}

/// The cells of a string ([`cells`]), each yielded once the next one opens
/// or the string ends: a character after a cell may still widen it.
pub(super) struct Cells<'a, V> {
    chars: std::str::CharIndices<'a>,
    walk: CellWalk<V>,
    /// The cell the walk is in, still open to the characters drawn in it.
    open: Option<Cell>,
}

impl<V: Host> Iterator for Cells<'_, V> {
    type Item = Cell;

    fn next(&mut self) -> Option<Cell> {
        for (at, ch) in self.chars.by_ref() {
            match self.walk.step(ch) {
                CellStep::Opens(width) => {
                    if let Some(done) = self.open.replace(Cell { at, ch, width }) {
                        return Some(done);
                    }
                }
                CellStep::Joins(grew) => {
                    if let Some(cell) = self.open.as_mut() {
                        cell.width += grew;
                    }
                }
                CellStep::Dropped => {}
            }
        }
        self.open.take()
    }
}

/// Display width in cells: the sum of [`cells`]. ASCII is its length (each
/// printable character is one cell of its own), without the walk.
fn width<V: Host>(s: &str) -> usize {
    if s.is_ascii() {
        return s.len();
    }
    cells::<V>(s).map(|c| c.width).sum()
}

/// `s` in at most `max` cells ([`cells`]), the last one `…` when it was cut:
/// cut BETWEEN two cells, never inside one, so a mark stays with its letter
/// and a wide glyph is never halved. `max == 0` is the empty string.
fn truncate_cells<V: Host>(s: &str, max: usize) -> String {
    if s.is_ascii() {
        return truncate(s, max);
    }
    if width::<V>(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    // The first cell that would leave no room for the `…`.
    let mut used = 0usize;
    let end = cells::<V>(s)
        .find(|c| {
            used += c.width;
            used > max - 1
        })
        .map_or(s.len(), |c| c.at);
    let mut t = s[..end].to_string();
    t.push('\u{2026}');
    t
}

/// The row's OWN vocabulary — the glyphs that spell a hand, a hold, mail, the
/// link, a story, a trust verdict, a reset — and its two separators (two
/// spaces between slots, ` · ` between clauses). None of it may arrive inside
/// a free-text value: a `meta role` of `⊘ hold pause ·fleet🔒  ◂ manager` would
/// otherwise print as a hold and a hand the session does not have.
const BAND_GLYPHS: &[char] = &[
    '\u{25c2}',
    '\u{25b8}',
    '\u{2298}',
    '\u{2709}',
    '\u{21af}',
    '\u{2191}',
    '\u{27df}',
    '~',
    '\u{2715}',
    '\u{25c7}',
    '\u{2713}',
    '\u{2717}',
    '\u{26a0}',
    '\u{2190}',
    '\u{2192}',
    '\u{00b7}',
    '\u{1f512}',
];

/// A wire-safe token: printable, single-spaced, none of the row's own glyphs,
/// at most `cap` cells of the host's grid, cut with `…` between two cells
/// (`truncate_cells`; a CJK token of `cap` characters would take twice the
/// room). Each cell holds what the grid's cell holds, its opening character
/// and at most 16 more, and a zero-width character with no cell to join is
/// dropped (`CellWalk`), so a token is at most `17 × cap` characters
/// whatever its input. Every free-text value the row prints (a role, a
/// holder, a sender, a reason, a reset time, a told story's text) passes
/// here, so a control byte, a bidi override, an invisible character or the
/// row's grammar in an agent-chosen name can never reach the chrome or
/// forge a slot.
///
/// The strip is the band's own (`text::is_stripped`, ruling
/// 2026-10-11, round fifteen content-7): this token once kept its own
/// shorter list, which passed the TAG characters (invisible ASCII an agent
/// reading `chrome` still reads), ZWSP, WJ, the BOM, U+061C, the soft hyphen
/// and U+180E that its siblings strip.
#[must_use]
pub fn sanitize_token<V: Host>(s: &str, cap: usize) -> String {
    let mut clean = String::with_capacity(s.len());
    let mut walk = CellWalk::<V>::new();
    let mut used = 0usize;
    let mut at_space = true;
    for c in s.chars() {
        let c = if c.is_whitespace() { ' ' } else { c };
        if is_stripped(c) || BAND_GLYPHS.contains(&c) || (c == ' ' && at_space) {
            continue;
        }
        // The token's own cells, as the grid holds them: what the grid
        // would keep nothing of is not kept.
        match walk.step(c) {
            CellStep::Dropped => continue,
            CellStep::Opens(cols) | CellStep::Joins(cols) => used += cols,
        }
        at_space = c == ' ';
        clean.push(c);
    }
    let token = clean.trim_end();
    // The one trailing space `trim_end` drops was a cell of its own.
    if used - (clean.len() - token.len()) <= cap {
        return token.to_string();
    }
    truncate_cells::<V>(token, cap)
}

/// A hold's reason as every human-facing surface prints and speaks it: the
/// wire token (`status hold=`'s pct-encoded form, `main%20broken`) DECODED to
/// its words ([`Host::pct_decode`]) and then held to [`sanitize_token`]'s
/// rule — a bridge-supplied reason can encode a control byte or a bidi
/// override, and the decode must not be the step that lets it through.
#[must_use]
pub fn hold_reason_words<V: Host>(reason: &str) -> String {
    sanitize_token::<V>(&V::pct_decode(reason), 32)
}

/// A duration as the row prints it: `12s`, `3m12s`, `2h05m`, `1d 22h`.
#[must_use]
pub fn fmt_dur(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else if s < 86_400 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else {
        format!("{}d {}h", s / 86_400, (s % 86_400) / 3600)
    }
}

/// The short form of a session id for the hand slot: `s-1e91`.
#[must_use]
pub fn short_sid(sid: &str) -> String {
    let n = sid.chars().count().min(6);
    sid.chars().take(n).collect()
}

/// The trust glyph: ✓ for a verdict the receiver trusts, ✗ for one it refuses,
/// ? for anything unresolved. BEFORE the sender, always.
#[must_use]
pub(crate) fn trust_glyph(trust: &str) -> char {
    match trust {
        "agent" | "human" | "owner" | "operator" | "verified" => '\u{2713}',
        "forged-self" | "unreadable" | "observer" | "refused" | "spoof" => '\u{2717}',
        _ => '?',
    }
}

/// A box's kind in a person's words: the kind before the colon of
/// `kind[:verdict]` (`bash:not-read-only` → `bash`), the hyphenated kinds
/// spelled out (`plan-exit` → `plan`). `None` for a box of no named kind.
#[must_use]
pub fn prompt_kind_words(detail: Option<&str>) -> Option<&str> {
    let kind = detail
        .and_then(|d| d.split(':').next())
        .filter(|k| !k.is_empty() && *k != "other")?;
    Some(match kind {
        "plan-enter" => "plan mode",
        "plan-exit" => "plan",
        "held-message" => "held message",
        "goal-proposal" => "goal",
        "computer-use" => "computer use",
        "read-outside-setting" => "outside read",
        "model-switch" => "model switch",
        "rate-nudge" => "rate-limit model",
        "model-pick" => "model picker",
        "goal-resume" => "paused goal",
        "trust" => "folder trust",
        "powershell" => "PowerShell",
        other => other,
    })
}

/// A box in the row's words: `bash approval`, `plan approval`; the agent's
/// question tool is a `question`, never an approval; a box of no named kind
/// is `approval`.
#[must_use]
pub fn prompt_band_word(detail: Option<&str>) -> String {
    match prompt_kind_words(detail) {
        Some("question") => "question".to_string(),
        Some(kind) => format!("{kind} approval"),
        None => "approval".to_string(),
    }
}

/// The phase slot for a published input stall — `(phase, since clauses,
/// spoken)`, the three things [`words`] composes a phase from:
///
/// ```text
/// frozen 2m03s · not reading input     frozen, not reading input for 2m03s; restart it
/// frozen 2m03s · survived its restart  frozen, still running after its restart signal; end it with signal kill
/// stopped 41s · input queued           stopped with input queued for 41s; resume it
/// ```
///
/// Where the host would carry the restart (`restart`, [`Host::stall_restart`])
/// the spoken remedy names it, the survivor's too — `…; restart it with aterm
/// ctl @<sid> restart pid=<leader> if-stalled` — since the host's act ends a
/// survivor itself and relaunches it, where the bare signal's exit reads as
/// the program's own.
///
/// It takes the slot AHEAD of typed attention: an "answer this box" over a
/// program that reads no input asks for an answer it cannot take. The
/// duration counts from the oldest unread byte (`since`).
///
/// [`Host::stall_restart`]: super::Host::stall_restart
#[must_use]
pub(crate) fn stall_words(
    since: Instant,
    stopped: bool,
    survived: bool,
    restart: Option<&str>,
    now: Instant,
) -> (String, Vec<String>, String) {
    let dur = fmt_dur(now.saturating_duration_since(since));
    if stopped {
        return (
            "stopped".to_string(),
            vec![dur.clone(), "input queued".to_string()],
            format!("stopped with input queued for {dur}; resume it"),
        );
    }
    let carried = |cmd: &str| format!("restart it with {cmd}");
    if survived {
        // The program lived through its restart signal.
        return (
            "frozen".to_string(),
            vec![dur, "survived its restart".to_string()],
            format!(
                "frozen, still running after its restart signal; {}",
                restart.map_or_else(|| "end it with signal kill".to_string(), carried)
            ),
        );
    }
    (
        "frozen".to_string(),
        vec![dur.clone(), "not reading input".to_string()],
        format!(
            "frozen, not reading input for {dur}; {}",
            restart.map_or_else(|| "restart it".to_string(), carried)
        ),
    )
}

/// Compose the six slots for `slot` as seen from a window whose story
/// watermark is `watermark`. Pure; allocates (it is called on CHANGE, never per
/// frame).
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "the six slots in their printed order, each beside its spoken twin; the story and hand arms are already helpers, and a further split would part the order from the twins"
)]
pub fn words<V: Host>(slot: &Slot<V>, now: Instant, watermark: u64) -> Words {
    // The row's words follow the level it is shown at (ruling 280): an
    // attention another place tells must not hide the story under it.
    let level = slot.shown_level(watermark);
    let tone = slot.tone(now, watermark);
    let role = slot
        .role
        .as_deref()
        .map_or_else(|| DASH.to_string(), |r| sanitize_token::<V>(r, 64));

    // phase + since. A TOLD point (`ctl story`) takes the slot for three
    // seconds ahead of everything: it is the watcher's decision, and the
    // phase it interrupts is still one keystroke of patience away.
    let (phase, mut since, mut spoken_phase) = if let Some((verb, text)) = slot.told_now(now)
        && let Some((glyph, word)) = verb.told_words()
    {
        let since = if text.is_empty() {
            Vec::new()
        } else {
            vec![text.to_string()]
        };
        let teller = verb.teller();
        let spoken = if text.is_empty() {
            format!("{word} by {teller}")
        } else {
            format!("{word} by {teller}, {text}")
        };
        (format!("{glyph} {word}"), since, spoken)
    } else if let Some(fact) = &slot.input_stall {
        stall_words(
            V::stall_since(fact),
            V::stall_stopped(fact),
            V::stall_survived(fact),
            V::stall_restart(fact),
            now,
        )
    } else if let Some(text) = slot
        .attention
        .as_ref()
        .filter(|_| !slot.attention_told_elsewhere)
    {
        let t = sanitize_token::<V>(text, 48);
        (t.clone(), Vec::new(), format!("attention, {t}"))
    } else if level == Level::Story {
        story_phase(slot, now, watermark)
    } else if let Some(agent) = slot
        .agent
        .as_ref()
        // A phase the reader could not name is no phase to print (ruling
        // 366): the row falls through to the shell's word or the dash;
        // `status phase=unknown` keeps the fact.
        .filter(|a| !matches!(a.phase, AgentPhase::Unknown))
    {
        let word = agent.phase.band_word();
        let mut clauses = Vec::new();
        let mut spoken = word.to_string();
        match &agent.phase {
            AgentPhase::Prompt { detail } => {
                let word = prompt_band_word(detail.as_deref());
                clauses.push(fmt_dur(now.saturating_duration_since(slot.agent_since)));
                spoken.clone_from(&word);
                (word, clauses, spoken)
            }
            AgentPhase::Wall { kind, reset, until } => {
                // `limited → 19:30 · 1d 22h`: the reset the notice named, then
                // the time TO it (design §1; the mock counts down) — never the
                // time since the limit began, which is the story's to tell.
                // A wall the harness retries prints its NEXT TRY the same way
                // (`can't reach the API → 14:05 · 3m00s`) and speaks it as one
                // — while it is ahead: once its time has come (`until`
                // reached) the try is made or the loop is not making it
                // (parked behind a hold, restarting, the machine asleep), and
                // the wall names its cause alone again. A reset a wall names
                // itself keeps its arrow after the countdown ends.
                let next_try_past = V::wall_retries(*kind) && until.is_some_and(|u| u <= now);
                if let Some(r) = reset.as_ref().filter(|_| !next_try_past) {
                    clauses.push(format!("\u{2192} {r}"));
                    spoken = if V::wall_retries(*kind) {
                        format!("{word}, next try {r}")
                    } else {
                        format!("{word}, resets {r}")
                    };
                }
                if let Some(left) = until
                    .map(|u| u.saturating_duration_since(now))
                    .filter(|d| !d.is_zero())
                {
                    clauses.push(fmt_dur(left));
                }
                // Who answers it (`WallAnswer`): the row is the record that the
                // harness resumes the wall, and where a person can lift it
                // faster, the ONE thing to do — calm words, never an alert.
                match slot.wall_answer {
                    WallAnswer::Nobody => {}
                    WallAnswer::Harness => spoken.push_str(", resumes by itself"),
                    WallAnswer::HarnessAsks(action) => {
                        if reset.is_none() {
                            clauses.push(format!("\u{2192} {action}"));
                        }
                        let _ = write!(spoken, ", resumes by itself, or sooner: {action}");
                    }
                }
                (word.to_string(), clauses, spoken)
            }
            _ => {
                clauses.push(fmt_dur(now.saturating_duration_since(slot.agent_since)));
                (word.to_string(), clauses, spoken)
            }
        }
    } else if let Some((word, at)) = slot.shell {
        (
            word.to_string(),
            vec![fmt_dur(now.saturating_duration_since(at))],
            word.to_string(),
        )
    } else {
        (DASH.to_string(), Vec::new(), String::new())
    };

    // hand (and its short form for a narrow row)
    let (hand, hand_short, spoken_hand) = hand_words(slot);
    if slot.hold.is_some() {
        // Under a hold the stop clause rides `since` so the summary law and the
        // live row agree on where a duration is printed — but not behind a
        // told word (`✓ approved 0s ⊘ hold review` read as an approval's age).
        if let Some((_, began)) = slot.stop_in_progress()
            && since.is_empty()
            && slot.told_now(now).is_none()
        {
            since.push(fmt_dur(now.saturating_duration_since(began)));
        }
    }

    // mail — no slot at all while there is none to tell (ruling 366): a
    // `✉0` is a count of nothing.
    let m = &slot.mail;
    let mut mail = if m.unread > 0 || m.pending > 0 || m.dropped > 0 || m.queued > 0 {
        format!("\u{2709}{}", m.unread)
    } else {
        String::new()
    };
    if m.pending > 0 {
        let _ = write!(mail, " \u{00b7}{}", m.pending);
    }
    if m.dropped > 0 {
        let _ = write!(mail, " \u{21af}{}", m.dropped);
    }
    if m.queued > 0 {
        let _ = write!(mail, " \u{2191}{}", m.queued);
    }
    let mail_short = mail.clone();
    let mut spoken_mail = String::new();
    if m.unread > 0 {
        spoken_mail = format!("{} unread", m.unread);
        if let Some(last) = &m.last {
            let kind = sanitize_token::<V>(&last.kind, 12);
            let from = sanitize_token::<V>(&last.from, 24);
            let _ = write!(mail, " {kind}\u{2190}{}{from}", trust_glyph(&last.trust));
            let _ = write!(spoken_mail, ", {kind} from {from}, {}", last.trust);
        }
    }

    // ctx: the context LEFT (`<n>% until auto-compact`, `<n>% context left`).
    let ctx = match slot.agent.as_ref().and_then(|a| a.context_pct) {
        Some(pct) if pct <= CTX_WARN_PCT => format!("ctx {pct}% left \u{26a0}"),
        Some(pct) => format!("ctx {pct}% left"),
        None => String::new(),
    };
    let spoken_ctx = slot
        .agent
        .as_ref()
        .and_then(|a| a.context_pct)
        .map(|p| format!("context {p} percent left"));

    // fabric
    let (fabric, spoken_fabric) = match slot.link {
        Link::Absent => ("\u{00b7}".to_string(), None),
        Link::Connected { rtt_ms: Some(ms) } => (format!("\u{27df} {ms}ms"), None),
        Link::Connected { rtt_ms: None } => ("\u{27df}".to_string(), None),
        // A stall with a date prints its age; one without (a bridge still
        // dialing since it attached) prints the glyph alone — never a figure
        // the model made up.
        Link::Stalled { age_ms: Some(a) } => {
            let s = a / 1000;
            (
                format!("~ {s}s"),
                Some(format!("bridge stalled {s} seconds")),
            )
        }
        Link::Stalled { age_ms: None } => ("~".to_string(), Some("bridge stalled".to_string())),
        Link::Disconnected => ("\u{2715} lost".to_string(), Some("bridge lost".to_string())),
    };

    if spoken_phase.is_empty() {
        spoken_phase = "quiet".to_string();
    }
    let mut sentence = Vec::new();
    if !spoken_hand.is_empty() {
        sentence.push(spoken_hand);
    }
    sentence.push(spoken_phase);
    if !spoken_mail.is_empty() {
        sentence.push(spoken_mail);
    }
    if let Some(c) = spoken_ctx.filter(|_| !ctx.is_empty()) {
        sentence.push(c);
    }
    if let Some(f) = spoken_fabric {
        sentence.push(f);
    }

    Words {
        role,
        phase,
        since,
        hand,
        hand_short,
        mail,
        mail_short,
        ctx,
        fabric,
        sentence: sentence.join(", "),
        tone,
    }
}

/// The phase slot at [`Level::Story`]: the summary of what happened after the
/// watermark (≤6 clauses, zero counts omitted): turns · timed out · mails ·
/// approvals · choices · questions · the longest stop that ended since. A
/// `choice` is the harness answering a question box by policy — counted apart
/// from the watcher's approvals, and apart from `question` (a box that
/// WAITED).
fn story_phase<V: Host>(
    slot: &Slot<V>,
    now: Instant,
    watermark: u64,
) -> (String, Vec<String>, String) {
    let count = |verb: StoryVerb| {
        slot.story_since(watermark)
            .filter(|p| p.verb == verb)
            .count()
    };
    let timeouts = count(StoryVerb::TurnTimedOut);
    let turns = count(StoryVerb::Turn) + timeouts;
    let mut story = Vec::new();
    for (n, word) in [
        (turns, "turn"),
        (timeouts, "timed out"),
        (count(StoryVerb::Mail), "mail"),
        (count(StoryVerb::Approval), "approval"),
        (count(StoryVerb::Chose), "choice"),
        (count(StoryVerb::Question), "question"),
    ] {
        if n > 0 {
            let plural = if n == 1 || word == "timed out" {
                ""
            } else {
                "s"
            };
            story.push(format!("{n} {word}{plural}"));
        }
    }
    // The longest stop that ENDED since the watermark: its verb, how long
    // it lasted, and how long ago it lifted.
    let mut stops: Vec<(StoryVerb, Instant, Instant)> = Vec::new();
    let mut open: Option<(StoryVerb, Instant)> = None;
    // A stop that LIFTED but never began in this story — it was already
    // standing when the slot was minted, so its length is unknown — is
    // told as the resume alone.
    let mut unpaired_resume: Option<Instant> = None;
    for p in slot.story_since(watermark) {
        match p.verb {
            StoryVerb::Hold | StoryVerb::Limited => open = Some((p.verb, p.at)),
            StoryVerb::Resumed => match open.take() {
                Some((v, began)) => stops.push((v, began, p.at)),
                None => unpaired_resume = Some(p.at),
            },
            _ => {}
        }
    }
    if let Some((verb, began, ended)) = stops
        .into_iter()
        .max_by_key(|(_, b, e)| e.saturating_duration_since(*b))
    {
        let word = if verb == StoryVerb::Hold {
            "held"
        } else {
            "limited"
        };
        story.push(format!(
            "{word} {}, resumed {} ago",
            fmt_dur(ended.saturating_duration_since(began)),
            fmt_dur(now.saturating_duration_since(ended))
        ));
    } else if let Some(ended) = unpaired_resume {
        story.push(format!(
            "resumed {} ago",
            fmt_dur(now.saturating_duration_since(ended))
        ));
    }
    // A worker still RUNNING is not quiet, story or no story: the live
    // phase keeps the slot (`busy 31s`) and the story rides `since` after
    // it — so a turn that timed out on a busy worker never reads `◇ quiet`.
    let live = match &slot.agent {
        Some(a) => matches!(a.phase, AgentPhase::Busy).then_some(("busy", slot.agent_since)),
        None => slot.shell.filter(|(w, _)| *w == "running"),
    };
    if let Some((word, at)) = live {
        let mut since = vec![fmt_dur(now.saturating_duration_since(at))];
        since.extend(story.iter().cloned());
        let spoken = if story.is_empty() {
            word.to_string()
        } else {
            format!("{word}, {}", story.join(", "))
        };
        (word.to_string(), since, spoken)
    } else {
        let mut clauses = Vec::new();
        if let Some(at) = slot.story_since(watermark).next().map(|p| p.at) {
            clauses.push(format!(
                "since {}",
                fmt_dur(now.saturating_duration_since(at))
            ));
        }
        clauses.extend(story);
        let spoken = format!("quiet, {}", clauses.join(", "));
        ("\u{25c7} quiet".to_string(), clauses, spoken)
    }
}

/// The hand slot, its short form for a narrow row, and its spoken twin.
fn hand_words<V: Host>(slot: &Slot<V>) -> (String, String, String) {
    match (&slot.hold, &slot.hand) {
        // aterm's own harness is named as aterm, never by its holder's pid
        // (ruling 313), on the rare row something else raised.
        (None, Hand::DrivenTurn { .. } | Hand::DrivenLease { .. }) if slot.aterm_hand => (
            "\u{25c2} aterm".to_string(),
            "\u{25c2} aterm".to_string(),
            "driven by aterm".to_string(),
        ),
        (Some(h), _) => {
            let reason = hold_reason_words::<V>(&h.reason);
            let fleet = if h.fleet {
                " \u{00b7}fleet\u{1f512}"
            } else {
                ""
            };
            (
                format!("\u{2298} hold {reason}{fleet}"),
                "\u{2298} hold".to_string(),
                format!(
                    "held, {reason}{}",
                    if h.fleet {
                        ", fleet, cannot be lifted here"
                    } else {
                        ""
                    }
                ),
            )
        }
        (
            None,
            Hand::DrivenTurn {
                id,
                holder: Some(h),
            },
        ) => {
            let h = sanitize_token::<V>(h, 32);
            (
                format!("\u{25c2} {h} \u{00b7} turn {id}"),
                format!("\u{25c2} turn {id}"),
                format!("driven by {h}, turn {id}"),
            )
        }
        (None, Hand::DrivenTurn { id, holder: None }) => (
            format!("\u{25c2} turn {id}"),
            format!("\u{25c2} turn {id}"),
            format!("driven, turn {id}"),
        ),
        (None, Hand::DrivenLease { holder }) => {
            let h = sanitize_token::<V>(holder, 32);
            (
                format!("\u{25c2} {h}"),
                format!("\u{25c2} {}", truncate_cells::<V>(&h, 8)),
                format!("driven by {h}"),
            )
        }
        (None, Hand::Driving { sid }) => {
            let s = sanitize_token::<V>(sid, 12);
            (
                format!("\u{25b8} @{s}"),
                format!("\u{25b8} @{s}"),
                format!("driving session {s}"),
            )
        }
        (None, Hand::None) => (DASH.to_string(), DASH.to_string(), String::new()),
    }
}
