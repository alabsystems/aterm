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
mod tests {
    use super::*;
    use aterm_grapheme::GraphemeClusters as _;

    /// A SNAPSHOT OF AN AGENT'S TITLE DROPS ITS SPINNER FRAME (the Together
    /// run's D7: `⠦ Add tested calculator functions | proj-codex` on the
    /// Settings ▸ Harness row, drawn as a blank indent): every braille frame
    /// and every circle or corner frame, generally — not one glyph.
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

        assert_eq!(chrome_presentation_text("abc", 0), "…");
        assert_eq!(chrome_presentation_text("abc", 1), "a…");
        assert_eq!(chrome_presentation_text("abc", 2), "a…c");
        assert_eq!(chrome_presentation_text("abc", 3), "abc");
        assert_eq!(
            chrome_presentation_text("  one\n\u{00ad}\u{200b}\u{202e}two\tthree\u{e0061}  ", 96),
            "one two three"
        );
    }

    /// THE DEEP-SIBLINGS DEFECT, at the presenter.
    #[test]
    fn the_strip_surface_presents_the_whole_title_and_the_window_surface_the_capped_one() {
        let cache = LabelCache::default();
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
        let accented = format!("{}\u{301}", leaf.0);
        let runs = cache.runs();
        let a = cache.present(Some(5), &accented, ChromeSurface::TabStrip);
        assert_eq!(a, accented);
        assert_eq!(cache.runs(), runs + 1, "presented, not fast-pathed");
        let again = cache.present(Some(5), &accented, ChromeSurface::TabStrip);
        assert_eq!(again, accented);
        assert_eq!(cache.runs(), runs + 1, "and cached");

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
        assert_eq!(&*capped_label(&projected), &*projected);
        let labels = vec![short.to_string(), "aterm".to_string()];
        assert!(matches!(capped_labels(&labels), Cow::Borrowed(_)));
        let labels = vec![short.to_string(), x(200)];
        let projected = capped_labels(&labels);
        assert!(matches!(projected, Cow::Owned(_)));
        assert_eq!(projected[0], short);
        assert_eq!(projected[1], expected);
    }
}
