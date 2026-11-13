// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The label a chrome surface paints for a session: its IDENTITY TITLE alone.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Mutex;
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};

/// What a surface paints when the resolved title sanitizes to nothing.
const FALLBACK: &str = "aterm";

/// Grapheme cap on a title where a chrome surface paints it WHOLE — the window titlebar ([`ChromeSurface::WindowTitle`]), and a strip label wherever it leaves the strip for a surface with no fit of its own ([`capped_label`]: the native toolbar's chip and its accessibility string, the tooltip / context-menu header, the a11y tab item, the introspection mirrors) — so a 1024-byte OSC title or a PATH_MAX cwd cannot become an enormous title.
pub(crate) const MAX_CHROME_TITLE_GRAPHEMES: usize = 96;

/// Which chrome surface a label is presented FOR — the one thing it decides is whether the title is capped, and it is the key the per-session [`LabelCache`] keeps each surface's label under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ChromeSurface {
    /// A tab-strip chip: handed to the strip WHOLE — no title cap — because the strip fits every label itself, by display cells and sibling-aware ([`MAX_CHROME_TITLE_GRAPHEMES`] tells the whole story).
    TabStrip,
    /// The window titlebar: painted whole, so the title takes [`MAX_CHROME_TITLE_GRAPHEMES`] by the middle cut.
    WindowTitle,
}

impl ChromeSurface {
    /// The title's grapheme cap on this surface — `None` where the surface
    /// fits the label itself.
    const fn title_cap(self) -> Option<usize> {
        match self {
            Self::TabStrip => None,
            Self::WindowTitle => Some(MAX_CHROME_TITLE_GRAPHEMES),
        }
    }
}

/// The uncached presentation of `raw_title` on `surface`: sanitized to one canonical line, middle-cut at the surface's cap where it has one, and `"aterm"` when nothing printable survives.
#[must_use]
pub(crate) fn present(raw_title: &str, surface: ChromeSurface) -> String {
    let title = match surface.title_cap() {
        Some(max_graphemes) => chrome_presentation_text(raw_title, max_graphemes),
        None => canonical_single_line(raw_title),
    };
    if title.is_empty() {
        FALLBACK.to_string()
    } else {
        title
    }
}

/// The per-session memo behind the per-frame label paths: the strip refill re-labels every tab on every redraw, and the window title is re-applied on every redraw too, so neither may sanitize (and allocate) a title that did not change.
#[derive(Debug, Default)]
pub(crate) struct LabelCache {
    entries: Mutex<HashMap<(u64, ChromeSurface), Presented>>,
    /// Full presentations performed (cache misses plus session-less calls).
    #[cfg(test)]
    runs: AtomicU64,
}

/// One memoized presentation: the raw title it was made from, and the label.
#[derive(Debug, Default)]
struct Presented {
    raw: String,
    label: String,
}

impl LabelCache {
    /// Present IN PLACE: `slot` arrives holding the raw resolved title and leaves holding the label `surface` paints for it.
    pub(crate) fn present_into(
        &self,
        session: Option<u64>,
        surface: ChromeSurface,
        slot: &mut String,
    ) {
        if !slot.is_empty() && title_is_presentation_clean(slot, surface.title_cap()) {
            return;
        }
        let Some(session) = session else {
            self.count_run();
            *slot = present(slot, surface);
            return;
        };
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(cached) = entries.get(&(session, surface))
            && cached.raw == *slot
        {
            slot.clone_from(&cached.label);
            return;
        }
        self.count_run();
        let label = present(slot, surface);
        let entry = entries.entry((session, surface)).or_default();
        entry.raw.clone_from(slot);
        entry.label = label;
        slot.clone_from(&entry.label);
    }

    /// [`Self::present_into`] as a fresh `String`, for the callers that own no resident slot (`App::tab_titles` builds a new vector each call).
    #[must_use]
    pub(crate) fn present(
        &self,
        session: Option<u64>,
        raw_title: &str,
        surface: ChromeSurface,
    ) -> String {
        let mut label = raw_title.to_string();
        self.present_into(session, surface, &mut label);
        label
    }

    /// Drop every slot `session` holds — its final view detached, so its id labels nothing any more.
    pub(crate) fn retire(&self, session: u64) {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(cached, _), _| *cached != session);
    }

    /// Whether any slot is held for `session`.
    #[cfg(test)]
    pub(crate) fn tracks(&self, session: u64) -> bool {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .any(|(cached, _)| *cached == session)
    }

    /// Number of full presentations performed so far.
    #[cfg(test)]
    pub(crate) fn runs(&self) -> u64 {
        self.runs.load(Ordering::Relaxed)
    }

    fn count_run(&self) {
        #[cfg(test)]
        self.runs.fetch_add(1, Ordering::Relaxed);
    }
}

/// True when the chrome sanitizer and the surface's grapheme cap (`title_cap`,
/// `None` for the strip) pass `title` through byte-identical, so the label may
/// keep it as-is.
fn title_is_presentation_clean(title: &str, title_cap: Option<usize>) -> bool {
    title_cap.is_none_or(|max_graphemes| title.len() <= max_graphemes)
        && !title.starts_with(' ')
        && !title.ends_with(' ')
        && !title.contains("  ")
        && title.bytes().all(|byte| (b' '..=b'~').contains(&byte))
}

/// Apply the spoof-resistant presentation policy used for authored session metadata, folding every whitespace run to one space and trimming the edges.
pub(crate) fn canonical_single_line(text: &str) -> String {
    let whitespace_normalized: String = text
        .chars()
        .map(|ch| if ch.is_whitespace() { ' ' } else { ch })
        .collect();
    let filtered = crate::session_timeline::sanitize_presentation_line(
        &whitespace_normalized,
        whitespace_normalized.len(),
    );
    let mut out = String::with_capacity(filtered.len());
    let mut pending_space = false;
    for ch in filtered.chars() {
        if ch == ' ' {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
        }
        pending_space = false;
        out.push(ch);
    }
    out
}

/// Sanitize `text` to one canonical line and cap it at `max_graphemes` grapheme clusters, cutting from the MIDDLE: the first `max - max / 2` clusters, one `…`, then the last `max / 2`.
pub(crate) fn chrome_presentation_text(text: &str, max_graphemes: usize) -> String {
    use aterm_grapheme::GraphemeClusters as _;

    let sanitized = canonical_single_line(text);
    if sanitized.len() <= max_graphemes {
        return sanitized;
    }
    let clusters: Vec<&str> = sanitized.graphemes().collect();
    if clusters.len() <= max_graphemes {
        return sanitized;
    }
    let head = max_graphemes - max_graphemes / 2;
    let tail = max_graphemes / 2;
    let mut out = String::with_capacity(sanitized.len());
    out.extend(clusters[..head].iter().copied());
    out.push('…');
    out.extend(clusters[clusters.len() - tail..].iter().copied());
    out
}

/// A strip label on its way to a surface that paints it WHOLE — no cell fit, no sibling-aware pass: the native toolbar's chip and its accessibility string, the tooltip / context-menu header, the a11y tab item, the `tabs` / `chrome` introspection lines.
pub(crate) fn capped_label(label: &str) -> Cow<'_, str> {
    use aterm_grapheme::GraphemeClusters as _;

    if label.len() <= MAX_CHROME_TITLE_GRAPHEMES
        || label.graphemes().nth(MAX_CHROME_TITLE_GRAPHEMES).is_none()
    {
        return Cow::Borrowed(label);
    }
    Cow::Owned(chrome_presentation_text(label, MAX_CHROME_TITLE_GRAPHEMES))
}

/// [`capped_label`] over a strip's worth of labels: borrowed whole when no label is past the cap (the ordinary strip — no allocation), else one owned vector with every label projected.
pub(crate) fn capped_labels(labels: &[String]) -> Cow<'_, [String]> {
    if labels
        .iter()
        .all(|label| matches!(capped_label(label), Cow::Borrowed(_)))
    {
        return Cow::Borrowed(labels);
    }
    Cow::Owned(
        labels
            .iter()
            .map(|label| capped_label(label).into_owned())
            .collect(),
    )
}

/// Whether `ch` is a frame of a busy spinner an agent animates at the head of its OSC title: any braille pattern (U+2800–U+28FF — every braille set of cli-spinners, Codex's `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏` among them), and the circle and corner sets (`◐◑◒◓◔◕`, Claude Code's moons; `◴◵◶◷`, `◰◱◲◳`, `◜◝◞◟◠◡`, `◢◣◤◥`).
#[must_use]
pub(crate) const fn is_spinner_frame(ch: char) -> bool {
    matches!(
        ch,
        '\u{2800}'..='\u{28FF}'
            | '\u{25D0}'..='\u{25D5}'
            | '\u{25F4}'..='\u{25F7}'
            | '\u{25F0}'..='\u{25F3}'
            | '\u{25DC}'..='\u{25E1}'
            | '\u{25E2}'..='\u{25E5}'
    )
}

/// AN AGENT'S TITLE SUBJECT WITHOUT ITS SPINNER FRAME: `title` past a leading frame ([`is_spinner_frame`]) and the blanks after it.
#[must_use]
pub(crate) fn without_spinner_frame(title: &str) -> &str {
    let mut chars = title.chars();
    match chars.next() {
        Some(frame) if is_spinner_frame(frame) => {
            let rest = chars.as_str();
            let subject = rest.trim_start();
            if subject.len() < rest.len() && !subject.is_empty() {
                subject
            } else {
                title
            }
        }
        _ => title,
    }
}

#[cfg(test)]
#[path = "chrome_label_tests.rs"]
mod tests;
