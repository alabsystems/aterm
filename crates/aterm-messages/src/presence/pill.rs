// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE NEEDS PILL'S MODEL (§5): what a window's titlebar pill says and offers
//! ([`Pill`]), and the one geometry its painters lay it out by and resolve a
//! click through ([`pill_layout`], [`PillLayout::hit`]): the macOS
//! `NeedsPill` view measures its pieces in points, the in-grid strip in grid
//! cells, and both hand the same pure function their measure. So a click
//! lands on the act the painter drew there, whatever the unit.

use super::NeedSeverity;
use super::words::PillWords;

/// What lit the pill.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PillKind {
    /// A need of this severity: the border's.
    Need(NeedSeverity),
    /// The person's own hold on the front session, nothing needing them.
    Yours,
}

/// The one act a pill offers beside going to the tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PillAct {
    /// Restart the front tab's frozen program: offered only where the host's
    /// restart act would be carried (`aterm-gui`'s `restart_act::offered`).
    Restart,
    /// Continue the front tab's stopped job (`signal cont`): the remedy the
    /// stall's own words name for a job stopped with input queued.
    Resume,
    /// Lift the person's hold (Take Over's other half).
    HandBack,
}

impl PillAct {
    /// The act's word on the pill.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Restart => "Restart",
            Self::Resume => "Resume",
            Self::HandBack => "Hand Back",
        }
    }
}

/// What a click on the pill does: the region it landed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PillHit {
    /// The body (the glyph, the label, the words): go to the tab.
    Go,
    /// The act.
    Act(PillAct),
    /// The `+N`: the host opens its Overview at the window's next need
    /// (`drive::next_need`).
    More,
}

/// One window's pill: the session it names and what it says of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pill {
    /// What lit it.
    pub kind: PillKind,
    /// The session it names (the host's local id): where the body goes.
    pub session: u64,
    /// The session's label (role, else user title, else the title the strip
    /// shows), sanitized; empty for the session in front, which the window
    /// names already — a background tab's, or another pane's of the front
    /// tab, is shown.
    pub label: String,
    /// The tab's 1-based number when it is not the window's front tab;
    /// `None` for the front tab.
    pub tab: Option<usize>,
    /// The need's words (the published verdict's, sanitized); empty for the
    /// person's hold.
    pub what: String,
    /// When the need began ([`super::Slot::need_since`]), on a background
    /// tab's pill alone, which prints its age (and whose age alone arms the
    /// host's tick). `None` for the front tab's pill and the person's hold.
    pub since: Option<crate::Instant>,
    /// The act it offers.
    pub act: Option<PillAct>,
    /// How many other sessions in the window need the person.
    pub more: usize,
}

/// One laid-out piece: from `x`, `w` wide, in the painter's unit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    /// Its leading edge.
    pub x: f64,
    /// Its width (0: not drawn).
    pub w: f64,
}

impl Span {
    const fn contains(self, x: f64) -> bool {
        self.w > 0.0 && x >= self.x && x < self.x + self.w
    }
}

/// The pill laid out at a width ([`pill_layout`]): each piece's span. The
/// glyph is never clipped; the words clip first, then the label; the act and
/// the count are whole or gone (the count first, then the act, only where
/// the glyph alone would not fit beside them).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PillLayout {
    /// The glyph.
    pub glyph: Span,
    /// The label (zero-wide for a front tab's pill).
    pub label: Span,
    /// The words.
    pub words: Span,
    /// The act, when drawn.
    pub act: Option<(PillAct, Span)>,
    /// The `+N`, when drawn.
    pub more: Option<Span>,
    /// The whole pill's width.
    pub width: f64,
}

impl PillLayout {
    /// What a click at `x` (the pill's own coordinate) does: the region the
    /// painter drew there, or `None` for the pill's margins.
    #[must_use]
    pub fn hit(&self, x: f64) -> Option<PillHit> {
        if let Some((act, span)) = self.act
            && span.contains(x)
        {
            return Some(PillHit::Act(act));
        }
        if self.more.is_some_and(|span| span.contains(x)) {
            return Some(PillHit::More);
        }
        let body_end = [self.glyph, self.label, self.words]
            .iter()
            .map(|s| s.x + s.w)
            .fold(self.glyph.x, f64::max);
        (x >= self.glyph.x && x < body_end).then_some(PillHit::Go)
    }
}

/// Lay `words` out in at most `max` (the painter's unit: points or cells),
/// each piece measured by `measure` and kept `gap` apart. The glyph keeps
/// its width whatever `max` is; the act and the `+N` are kept whole while
/// the glyph fits beside them (the `+N` given up first); what is left goes
/// to the label first and the words after, so the words clip first and the
/// label after them.
#[must_use]
pub fn pill_layout(
    words: &PillWords,
    max: f64,
    gap: f64,
    measure: impl Fn(&str) -> f64,
) -> PillLayout {
    let mut glyph_buf = [0u8; 4];
    let glyph_w = measure(words.glyph.encode_utf8(&mut glyph_buf));
    let mut act = words.act.map(|a| (a, measure(a.word())));
    let mut more = words.more.as_deref().map(&measure);
    let fixed = |act: &Option<(PillAct, f64)>, more: &Option<f64>| {
        glyph_w + act.map_or(0.0, |(_, w)| gap + w) + more.map_or(0.0, |w| gap + w)
    };
    if fixed(&act, &more) > max {
        more = None;
    }
    if fixed(&act, &more) > max {
        act = None;
    }
    let mut room = (max - fixed(&act, &more)).max(0.0);
    let label_full = if words.label.is_empty() {
        0.0
    } else {
        measure(&words.label)
    };
    let label_w = if label_full > 0.0 && room > gap {
        let w = label_full.min(room - gap);
        room -= gap + w;
        w
    } else {
        0.0
    };
    // The words' span is measured at their room ([`PillWords::room`]): as
    // wide as they get before the age's unit moves, so the pill's width holds
    // still while its age ticks.
    let words_full = measure(&words.room);
    let words_w = if words_full > 0.0 && room > gap {
        words_full.min(room - gap)
    } else {
        0.0
    };
    let mut x = 0.0;
    let glyph = Span { x, w: glyph_w };
    x += glyph_w;
    let mut place = |w: f64| {
        if w <= 0.0 {
            return Span { x, w: 0.0 };
        }
        x += gap;
        let span = Span { x, w };
        x += w;
        span
    };
    let label = place(label_w);
    let words_span = place(words_w);
    let act = act.map(|(a, w)| (a, place(w)));
    let more = more.map(&mut place);
    PillLayout {
        glyph,
        label,
        words: words_span,
        act,
        more,
        width: x,
    }
}
