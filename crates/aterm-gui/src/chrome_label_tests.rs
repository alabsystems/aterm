// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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

/// THE DEEP-SIBLINGS DEFECT, at the presenter. The strip's label used to
/// take the chrome cap before the strip's own sibling-aware pass — and any
/// fixed cut is sibling-blind: the head cut collapsed tabs that differ
/// only past grapheme 96 (the glass finding), and a middle cut collapses
/// tabs that differ only inside `[48, N-48)` (two worktree checkouts of
/// one repo, `wf_<id>-10` beside `wf_<id>-25`) that the head cut had told
/// apart.
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
