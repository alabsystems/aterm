// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The label a chrome surface paints for a session: its IDENTITY TITLE alone.
//!
//! Each surface resolves that title from its own ladder — the operator's `meta
//! set title` pin, then the program's OSC 0/2 title, then the reported cwd
//! (`App::tab_titles`, `App::refill_strip_titles`, `App::window_title_identity`)
//! — and hands the raw string here. This module only makes it presentable: one
//! line under the spoof-resistant presentation policy authored metadata gets
//! ([`crate::session_timeline::sanitize_presentation_line`]), and, on a surface
//! that paints the title whole, a grapheme cap. The durable title in its owner
//! (the meta leaf, the engine) is never rewritten.
//!
//! Every surface reads the same text: the in-grid strip, the native toolbar,
//! the OS window title, the a11y tab names, `inspect app/v1 tabs` and the
//! `chrome` verb. Until 2026-10-15 the label also carried a generated "what is
//! it doing" clause joined after the title (the retired Smart Titles feature);
//! that clause is meant to return as a consumer of the session log, never as a
//! second half joined into this string and parsed back apart.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Mutex;
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};

/// What a surface paints when the resolved title sanitizes to nothing.
const FALLBACK: &str = "aterm";

/// Grapheme cap on a title where a chrome surface paints it WHOLE — the window
/// titlebar ([`ChromeSurface::WindowTitle`]), and a strip label wherever it
/// leaves the strip for a surface with no fit of its own ([`capped_label`]:
/// the native toolbar's chip and its accessibility string, the tooltip /
/// context-menu header, the a11y tab item, the introspection mirrors) — so a
/// 1024-byte OSC title or a PATH_MAX cwd cannot become an enormous title. The
/// cut is a MIDDLE cut ([`chrome_presentation_text`]), `head…tail`: for a path
/// the tail is the half that says where it ends.
///
/// NOT the tab strip's. The strip fits every label itself, by display cells
/// and SIBLING-AWARE — `tab_bar::distinct_chip_labels` sheds the head and the
/// tail the clustered tabs share and keeps what tells them apart — and a fixed
/// cut taken here first, head or middle, is sibling-blind: it discards one
/// region of every title, so two tabs that differ only inside that region
/// reach the strip as one byte-identical string and paint one label, or
/// ordinals. Measured on glass with sibling directories deeper than the cap
/// (the head cut discards `[96, N)`), and traced again with two worktree
/// checkouts of one repo, `wf_<id>-10` beside `wf_<id>-25` with the id
/// sixty-nine graphemes in, which a middle cut (discarding `[48, N-48)`)
/// collapsed where the head cut had not. So [`ChromeSurface::TabStrip`]
/// presents with NO title cap: the strip's inputs are bounded upstream already
/// (`MAX_TITLE_BYTES` for an OSC title, `META_TITLE_MAX` for `meta set title`,
/// `MAX_CWD_PATH_BYTES` for a cwd), and its pairwise prefix/suffix scan over a
/// strip's worth of titles is trivial.
pub(crate) const MAX_CHROME_TITLE_GRAPHEMES: usize = 96;

/// Which chrome surface a label is presented FOR — the one thing it decides is
/// whether the title is capped, and it is the key the per-session
/// [`LabelCache`] keeps each surface's label under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ChromeSurface {
    /// A tab-strip chip: handed to the strip WHOLE — no title cap — because
    /// the strip fits every label itself, by display cells and sibling-aware
    /// ([`MAX_CHROME_TITLE_GRAPHEMES`] tells the whole story). A strip label
    /// that leaves the strip for a surface with no fit of its own goes through
    /// [`capped_label`] there.
    TabStrip,
    /// The window titlebar: painted whole, so the title takes
    /// [`MAX_CHROME_TITLE_GRAPHEMES`] by the middle cut.
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

/// The uncached presentation of `raw_title` on `surface`: sanitized to one
/// canonical line, middle-cut at the surface's cap where it has one, and
/// `"aterm"` when nothing printable survives.
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

/// The per-session memo behind the per-frame label paths: the strip refill
/// re-labels every tab on every redraw, and the window title is re-applied on
/// every redraw too, so neither may sanitize (and allocate) a title that did
/// not change.
///
/// - CLEAN FAST PATH: a title the sanitizer (and the surface's cap, where it
///   has one) would pass through unchanged IS the label — no sanitize or
///   grapheme pass, no allocation, no cache traffic.
/// - CACHE HIT: an unchanged raw title for this session and surface is served
///   from the stored label via `clone_from` into the caller's resident slot —
///   no fresh allocation after warm-up.
/// - CACHE MISS: [`present`] once, then store. The two surfaces occupy
///   separate keys so the two per-frame callers cannot evict each other.
///
/// Bounded by sessions × surfaces: [`Self::retire`] drops a session's slots
/// when its final view detaches (`App::detach_session_view`).
#[derive(Debug, Default)]
pub(crate) struct LabelCache {
    entries: Mutex<HashMap<(u64, ChromeSurface), Presented>>,
    /// Full presentations performed (cache misses plus session-less calls).
    /// Frame-path tests pin cache hits with this.
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
    /// Present IN PLACE: `slot` arrives holding the raw resolved title and
    /// leaves holding the label `surface` paints for it. `session` is the
    /// cache identity; `None` (a tab with no session) presents directly.
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

    /// [`Self::present_into`] as a fresh `String`, for the callers that own no
    /// resident slot (`App::tab_titles` builds a new vector each call).
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

    /// Drop every slot `session` holds — its final view detached, so its id
    /// labels nothing any more.
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
/// keep it as-is. Printable ASCII with single interior spaces and no edge
/// spaces is exactly identity under [`canonical_single_line`] (nothing
/// filtered, collapsed, or trimmed), and each such byte is one grapheme, so
/// the cap reduces to `len()`.
fn title_is_presentation_clean(title: &str, title_cap: Option<usize>) -> bool {
    title_cap.is_none_or(|max_graphemes| title.len() <= max_graphemes)
        && !title.starts_with(' ')
        && !title.ends_with(' ')
        && !title.contains("  ")
        && title.bytes().all(|byte| (b' '..=b'~').contains(&byte))
}

/// Apply the spoof-resistant presentation policy used for authored session
/// metadata, folding every whitespace run to one space and trimming the edges.
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

/// Sanitize `text` to one canonical line and cap it at `max_graphemes`
/// grapheme clusters, cutting from the MIDDLE: the first `max - max / 2`
/// clusters, one `…`, then the last `max / 2`. At most `max + 1` graphemes
/// leave here, and a text at or under the cap leaves byte-identical — the
/// identity [`title_is_presentation_clean`] relies on. Idempotent in value: a
/// text this cut already shaped is `max + 1` clusters with the mark seated
/// after the head, and cutting it again drops that mark and seats the same
/// one in its place.
///
/// The cut keeps the TAIL because for a path the tail is the half that says
/// where it ends ([`MAX_CHROME_TITLE_GRAPHEMES`]); both cuts land on grapheme
/// boundaries, so a combining mark, a ZWJ sequence or a flag stays whole on
/// either side of the mark.
pub(crate) fn chrome_presentation_text(text: &str, max_graphemes: usize) -> String {
    use aterm_grapheme::GraphemeClusters as _;

    let sanitized = canonical_single_line(text);
    // A cluster is at least one byte, so a text within the cap in BYTES is
    // within it in clusters: the ordinary short title never segments.
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

/// A strip label on its way to a surface that paints it WHOLE — no cell fit,
/// no sibling-aware pass: the native toolbar's chip and its accessibility
/// string, the tooltip / context-menu header, the a11y tab item, the `tabs` /
/// `chrome` introspection lines. The strip's presentation
/// ([`ChromeSurface::TabStrip`]) caps no title, so the cap those surfaces
/// always had is applied HERE: [`MAX_CHROME_TITLE_GRAPHEMES`] by the middle
/// cut. A label within the cap is borrowed unchanged, and the projection is
/// idempotent in value.
pub(crate) fn capped_label(label: &str) -> Cow<'_, str> {
    use aterm_grapheme::GraphemeClusters as _;

    if label.len() <= MAX_CHROME_TITLE_GRAPHEMES
        || label.graphemes().nth(MAX_CHROME_TITLE_GRAPHEMES).is_none()
    {
        return Cow::Borrowed(label);
    }
    Cow::Owned(chrome_presentation_text(label, MAX_CHROME_TITLE_GRAPHEMES))
}

/// [`capped_label`] over a strip's worth of labels: borrowed whole when no
/// label is past the cap (the ordinary strip — no allocation), else one owned
/// vector with every label projected.
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

/// Whether `ch` is a frame of a busy spinner an agent animates at the head
/// of its OSC title: any braille pattern (U+2800–U+28FF — every braille set
/// of cli-spinners, Codex's `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏` among them), and the circle and
/// corner sets (`◐◑◒◓◔◕`, Claude Code's moons; `◴◵◶◷`, `◰◱◲◳`, `◜◝◞◟◠◡`,
/// `◢◣◤◥`). A rest mark an agent keeps while idle (Claude Code's `✳`) is no
/// frame.
#[must_use]
pub(crate) fn is_spinner_frame(ch: char) -> bool {
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

/// AN AGENT'S TITLE SUBJECT WITHOUT ITS SPINNER FRAME: `title` past a leading
/// frame ([`is_spinner_frame`]) and the blanks after it. A surface that
/// shows the subject as a snapshot — a Settings ▸ Harness row, the
/// Autopilot menu's name, the menu bar's window rows, a session's name on a
/// card, a picker, a connection line or the connection map
/// (`SessionHandle::display_title`) — reads it through here, so it never freezes one frame
/// of an animation (a braille frame draws as a blank indent in a UI face:
/// the Together run's D7). The live chrome (the strip, the window title)
/// keeps the frame and coalesces its phases instead
/// (`toolbar::busy_spinner_phase_only_change`). A frame with no blank after
/// it, or with nothing after it, is the title's own text and stays.
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
mod tests {
    use super::*;
    use aterm_grapheme::GraphemeClusters as _;

    /// A SNAPSHOT OF AN AGENT'S TITLE DROPS ITS SPINNER FRAME (the Together
    /// run's D7: `⠦ Add tested calculator functions | proj-codex` on the
    /// Settings ▸ Harness row, drawn as a blank indent): every braille frame
    /// and every circle or corner frame, generally — not one glyph. NEGATIVE
    /// CONTROLS: Claude Code's rest mark `✳` stays; a frame glued to a word,
    /// a frame alone, a frame in the middle and a plain title are untouched.
    #[test]
    fn a_titles_leading_spinner_frame_is_dropped_and_nothing_else() {
        let subject = "Add tested calculator functions | proj-codex";
        for frame in ('\u{2800}'..='\u{28FF}').chain(
            "\u{25D0}\u{25D1}\u{25D2}\u{25D3}\u{25D4}\u{25D5}\u{25F4}\u{25F5}\u{25F6}\u{25F7}\
             \u{25F0}\u{25F1}\u{25F2}\u{25F3}\u{25DC}\u{25DD}\u{25DE}\u{25DF}\u{25E0}\u{25E1}\
             \u{25E2}\u{25E3}\u{25E4}\u{25E5}"
                .chars(),
        ) {
            let title = format!("{frame} {subject}");
            assert_eq!(without_spinner_frame(&title), subject, "{frame:?}");
            let title = format!("{frame}\u{a0} {subject}");
            assert_eq!(without_spinner_frame(&title), subject, "{frame:?}");
        }
        assert_eq!(
            without_spinner_frame("\u{25D1} Calc.py small tested module"),
            "Calc.py small tested module",
            "Claude Code's busy moon"
        );
        for kept in [
            "\u{2733} Calc.py small tested module",
            "\u{2733} Claude Code",
            "\u{2826}Add",
            "\u{2826}",
            "\u{2826}  ",
            "proj \u{2826} codex",
            "Add tested calculator functions",
            "",
        ] {
            assert_eq!(without_spinner_frame(kept), kept, "{kept:?}");
        }
    }

    /// Where the mark sits for a cap of 96: after the 48-cluster head.
    const MARK: usize = MAX_CHROME_TITLE_GRAPHEMES - MAX_CHROME_TITLE_GRAPHEMES / 2;

    #[test]
    fn the_window_cap_cuts_the_middle_on_grapheme_boundaries() {
        // A tail that names itself, so the pin below is about the tail
        // SURVIVING the cap, not merely about the count.
        let long = format!("{}TAIL", "x".repeat(1024));
        let projected = present(&long, ChromeSurface::WindowTitle);
        assert_eq!(projected.graphemes().count(), 97);
        assert_eq!(projected.graphemes().nth(MARK), Some("…"));
        assert!(projected.starts_with(&"x".repeat(MARK)), "{projected:?}");
        assert!(projected.ends_with("xTAIL"), "{projected:?}");

        let combining = "e\u{301}".repeat(97);
        let projected = chrome_presentation_text(&combining, 96);
        assert_eq!(projected.graphemes().count(), 97);
        assert_eq!(projected.graphemes().nth(MARK - 1), Some("e\u{301}"));
        assert_eq!(projected.graphemes().nth(MARK), Some("…"));
        assert_eq!(projected.graphemes().nth(MARK + 1), Some("e\u{301}"));
        assert_eq!(projected.graphemes().last(), Some("e\u{301}"));
        assert_eq!(
            projected.graphemes().filter(|g| *g == "e\u{301}").count(),
            96,
            "both halves are whole clusters: {projected:?}"
        );

        let family = "👩\u{200d}👩\u{200d}👧\u{200d}👦";
        let projected = chrome_presentation_text(&family.repeat(97), 96);
        assert_eq!(projected.graphemes().count(), 97);
        assert_eq!(projected.graphemes().nth(MARK - 1), Some(family));
        assert_eq!(projected.graphemes().nth(MARK), Some("…"));
        assert_eq!(projected.graphemes().nth(MARK + 1), Some(family));
        assert_eq!(projected.graphemes().last(), Some(family));
        assert_eq!(projected.graphemes().filter(|g| *g == family).count(), 96);

        // The degenerate caps keep the envelope (`max + 1` graphemes at most)
        // and a text AT the cap is identity.
        assert_eq!(chrome_presentation_text("abc", 0), "…");
        assert_eq!(chrome_presentation_text("abc", 1), "a…");
        assert_eq!(chrome_presentation_text("abc", 2), "a…c");
        assert_eq!(chrome_presentation_text("abc", 3), "abc");
        assert_eq!(
            chrome_presentation_text("  one\n\u{00ad}\u{200b}\u{202e}two\tthree\u{e0061}  ", 96),
            "one two three"
        );
    }

    /// THE DEEP-SIBLINGS DEFECT, at the presenter. The strip's label used to
    /// take the chrome cap before the strip's own sibling-aware pass — and any
    /// fixed cut is sibling-blind: the head cut collapsed tabs that differ
    /// only past grapheme 96 (the glass finding), and a middle cut collapses
    /// tabs that differ only inside `[48, N-48)` (two worktree checkouts of
    /// one repo, `wf_<id>-10` beside `wf_<id>-25`) that the head cut had told
    /// apart. So the strip surface caps no title: the strip label IS the
    /// sanitized title — through the cached per-session path `App::tab_titles`
    /// takes — while the window surface, which paints the title whole, still
    /// takes the cap.
    #[test]
    fn the_strip_surface_presents_the_whole_title_and_the_window_surface_the_capped_one() {
        let cache = LabelCache::default();
        // One sibling pair per region a fixed cut discards: the leaf, past
        // the head cut's window; the worktree id, inside the middle cut's.
        let leaf = (
            format!("~/{}deep-one", "ab/".repeat(40)),
            format!("~/{}deep-two", "ab/".repeat(40)),
        );
        let worktree = (
            "~/src/github.com/alabsystems/aterm/.claude/worktrees/lane-bec3123b-89c-10/crates/aterm-gui/src/native_config/fixtures/model".to_string(),
            "~/src/github.com/alabsystems/aterm/.claude/worktrees/lane-bec3123b-89c-25/crates/aterm-gui/src/native_config/fixtures/model".to_string(),
        );
        let head_cut = |title: &str| {
            let head: String = title.graphemes().take(MAX_CHROME_TITLE_GRAPHEMES).collect();
            format!("{head}…")
        };
        let middle_cut = |title: &str| chrome_presentation_text(title, MAX_CHROME_TITLE_GRAPHEMES);
        // FIXTURE GUARDS, and the two sibling-blind cuts as negative controls:
        // each collapses a pair the other tells apart.
        for (one, two) in [&leaf, &worktree] {
            assert!(
                one.graphemes().count() > MAX_CHROME_TITLE_GRAPHEMES,
                "{one:?}"
            );
            assert_eq!(one.graphemes().count(), two.graphemes().count());
        }
        assert_eq!(
            head_cut(&leaf.0),
            head_cut(&leaf.1),
            "the head cut collapses the leaf pair"
        );
        assert_ne!(middle_cut(&leaf.0), middle_cut(&leaf.1));
        assert_ne!(
            head_cut(&worktree.0),
            head_cut(&worktree.1),
            "the head cut told the worktrees apart"
        );
        assert_eq!(
            middle_cut(&worktree.0),
            middle_cut(&worktree.1),
            "the middle cut collapses the worktree pair"
        );

        for (session, (one, two)) in [(1, &leaf), (3, &worktree)] {
            let a = cache.present(Some(session), one, ChromeSurface::TabStrip);
            let b = cache.present(Some(session + 1), two, ChromeSurface::TabStrip);
            assert_eq!(&a, one, "the strip receives the whole title");
            assert_eq!(&b, two, "the strip receives the whole title");
        }
        // Through the SANITIZING path too (a title the clean fast path
        // declines — a combining mark on the leaf): the sanitizer runs, the
        // cache fills, and the title is still whole.
        let accented = format!("{}\u{301}", leaf.0);
        let runs = cache.runs();
        let a = cache.present(Some(5), &accented, ChromeSurface::TabStrip);
        assert_eq!(a, accented);
        assert_eq!(cache.runs(), runs + 1, "presented, not fast-pathed");
        let again = cache.present(Some(5), &accented, ChromeSurface::TabStrip);
        assert_eq!(again, accented);
        assert_eq!(cache.runs(), runs + 1, "and cached");

        // The window surface paints the title whole, so it keeps the cap:
        // `head…tail`, the mark seated after 48 clusters, the leaf surviving.
        let window = cache.present(Some(1), &leaf.0, ChromeSurface::WindowTitle);
        assert_eq!(window, middle_cut(&leaf.0));
        assert_eq!(window.graphemes().count(), MAX_CHROME_TITLE_GRAPHEMES + 1);
        assert_eq!(window.graphemes().nth(MARK), Some("…"));
        assert!(window.starts_with("~/ab/"), "{window:?}");
        assert!(window.ends_with("deep-one"), "{window:?}");
    }

    /// The per-frame contract: an unchanged title is a cache hit on each
    /// surface, the two surfaces never evict each other, a title change
    /// presents exactly once, and retirement drops the session's slots.
    #[test]
    fn labels_cache_per_session_and_surface_until_the_title_changes() {
        let cache = LabelCache::default();
        // A spinner glyph: not presentation-clean, so it takes the cache.
        let first = cache.present(Some(7), "✳ make", ChromeSurface::TabStrip);
        assert_eq!(first, "✳ make");
        let runs = cache.runs();
        assert_eq!(runs, 1);
        assert_eq!(
            cache.present(Some(7), "✳ make", ChromeSurface::TabStrip),
            first
        );
        assert_eq!(cache.runs(), runs, "an unchanged title reuses the label");

        let window = cache.present(Some(7), "✳ make", ChromeSurface::WindowTitle);
        assert_eq!(window, "✳ make");
        let warm = cache.runs();
        assert_eq!(
            cache.present(Some(7), "✳ make", ChromeSurface::TabStrip),
            first
        );
        assert_eq!(
            cache.present(Some(7), "✳ make", ChromeSurface::WindowTitle),
            window
        );
        assert_eq!(cache.runs(), warm, "surfaces are cached independently");

        assert_eq!(
            cache.present(Some(7), "✳ make  check", ChromeSurface::TabStrip),
            "✳ make check"
        );
        assert_eq!(cache.runs(), warm + 1, "a title change presents once");

        assert!(cache.tracks(7));
        cache.retire(7);
        assert!(!cache.tracks(7), "retirement drops every surface's slot");
    }

    /// The clean-title fast path must be indistinguishable from the full
    /// sanitize/grapheme presentation on BOTH surfaces, and must reject every
    /// title the sanitizer — or the surface's cap — would actually rewrite.
    #[test]
    fn clean_title_fast_path_matches_full_presentation() {
        let cache = LabelCache::default();
        let long_clean = "x".repeat(MAX_CHROME_TITLE_GRAPHEMES);
        let clean = ["vim", "cargo build --release", "a b c", long_clean.as_str()];
        let over_cap = "y".repeat(MAX_CHROME_TITLE_GRAPHEMES + 1);
        let dirty = [
            " padded ",
            "two  spaces",
            "tab\there",
            "combining e\u{301}",
            "bidi \u{202e}spoof",
        ];
        for surface in [ChromeSurface::TabStrip, ChromeSurface::WindowTitle] {
            let cap = surface.title_cap();
            for title in clean {
                assert!(
                    title_is_presentation_clean(title, cap),
                    "{title:?} on {surface:?}"
                );
            }
            for title in dirty {
                assert!(
                    !title_is_presentation_clean(title, cap),
                    "{title:?} on {surface:?}"
                );
            }
        }
        // The cap is the SURFACE's: one past it is dirty for the window
        // title, which paints it whole, and clean for the strip, which fits
        // the label itself and must see every grapheme.
        assert!(!title_is_presentation_clean(
            &over_cap,
            ChromeSurface::WindowTitle.title_cap()
        ));
        assert!(title_is_presentation_clean(
            &over_cap,
            ChromeSurface::TabStrip.title_cap()
        ));
        for surface in [ChromeSurface::TabStrip, ChromeSurface::WindowTitle] {
            for title in clean
                .iter()
                .chain(dirty.iter())
                .copied()
                .chain([over_cap.as_str()])
            {
                assert_eq!(
                    cache.present(None, title, surface),
                    present(title, surface),
                    "{title:?} on {surface:?}"
                );
            }
        }
        for surface in [ChromeSurface::TabStrip, ChromeSurface::WindowTitle] {
            assert_eq!(
                cache.present(None, "", surface),
                "aterm",
                "an empty title still resolves the fallback"
            );
            assert_eq!(
                cache.present(Some(1), "\u{202e}\u{200b}", surface),
                "aterm",
                "a title that sanitizes to nothing resolves the fallback"
            );
        }
    }

    /// A strip label on its way to a surface that paints it WHOLE takes the
    /// cap there by the middle cut; a label within it is borrowed.
    #[test]
    fn a_capped_label_borrows_within_the_cap_and_middle_cuts_past_it() {
        let x = |n: usize| "x".repeat(n);
        let short = "vim README.md";
        assert!(matches!(capped_label(short), Cow::Borrowed(_)));
        assert!(matches!(
            capped_label(&x(MAX_CHROME_TITLE_GRAPHEMES)),
            Cow::Borrowed(_)
        ));
        let expected = format!("{}…{}", x(48), x(48));
        let long = x(200);
        let projected = capped_label(&long);
        assert_eq!(&*projected, expected.as_str());
        // Idempotent in value.
        assert_eq!(&*capped_label(&projected), &*projected);
        // A strip's worth: borrowed while every label is within the cap.
        let labels = vec![short.to_string(), "aterm".to_string()];
        assert!(matches!(capped_labels(&labels), Cow::Borrowed(_)));
        let labels = vec![short.to_string(), x(200)];
        let projected = capped_labels(&labels);
        assert!(matches!(projected, Cow::Owned(_)));
        assert_eq!(projected[0], short);
        assert_eq!(projected[1], expected);
    }
}
