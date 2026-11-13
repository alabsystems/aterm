// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use aterm_core::terminal::{HolderView, MAX_HOLDER_SPANS};
use atpkg::image::{ImageBuild, ImageSource, ProgramTag};

use crate::claude_card::FenceFrame;
use crate::holder_image::{CARD_PROBE_BATCHES, CardProgram, EdgeKind, HolderImage};
use crate::session_timeline::SessionTimeline;

/// Claude Code 2.1.304's inline card and composer as a screen reads them.
fn card_screen() -> Vec<String> {
    let rule = "\u{2500}".repeat(120);
    let mut screen = vec![
        String::new(),
        format!(
            " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}   {}2.1.304",
            aterm_phase::anchor!("launch.banner")
        ),
        "\u{259D}\u{259C}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2580}  Opus 5.5 \u{b7} Claude Max"
            .to_string(),
        " \u{259D}\u{259D}   \u{259D}\u{259D}   ~/aterm".to_string(),
        String::new(),
        rule.clone(),
        "\u{276F} ".to_string(),
        rule,
        "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)".to_string(),
    ];
    screen.resize(24, String::new());
    screen
}

/// A timeline whose holder is Claude Code 2.1.304's store image, and its token.
fn drawer_timeline() -> (Mutex<SessionTimeline>, u64) {
    let timeline = Mutex::new(SessionTimeline::default());
    let image = HolderImage::of(
        80,
        Some(41),
        Some(PathBuf::from(
            "/p/store/claude/1000002000001000304/bin/claude",
        )),
        ImageBuild {
            program: ProgramTag::new("claude"),
            build: 1_000_002_000_001_000_304,
            source: ImageSource::Store,
        },
        1,
    );
    let token = image.token.expect("tracked");
    timeline
        .lock()
        .unwrap()
        .offer_holder_image(Arc::new(image), EdgeKind::Group);
    (timeline, token)
}

/// The main screen at absolute row `base`, the drawer `token`'s open span starting at
/// absolute row `start`, before a slice with `mutations` screen mutations.
fn fence(token: u64, start: u64, base: i64, mutations: u64) -> FenceFrame {
    let mut spans = [(0, 0); MAX_HOLDER_SPANS];
    spans[0] = (start, u64::MAX);
    FenceFrame {
        holder: HolderView {
            token,
            n: 1,
            spans,
            tainted: false,
            alt_fresh: false,
        },
        base_y: base,
        alt: false,
        mutations,
    }
}

/// The `card-seen` rows' payloads.
fn seen_rows(timeline: &Mutex<SessionTimeline>) -> Vec<String> {
    timeline
        .lock()
        .unwrap()
        .since(None)
        .filter(|e| e.kind == "card-seen")
        .map(|e| e.payload.to_string())
        .collect()
}

/// The card is dated by the mutations before the slice it was first read in, and a batch
/// spends one of the budget's batches however many slices it took.
#[test]
fn the_probe_dates_the_card_by_the_slice_that_drew_it() {
    let (timeline, token) = drawer_timeline();
    let (mut budget, mut probed_batch) = (None, false);
    let blank = vec![String::new(); 24];
    super::note_card_seen_in(
        7,
        &timeline,
        &mut budget,
        &mut probed_batch,
        (&blank, &fence(token, 100, 100, 4)),
    );
    assert_eq!(
        timeline.lock().unwrap().card_unseen(),
        Some((token, CardProgram::Claude)),
        "no card yet"
    );
    super::note_card_seen_in(
        7,
        &timeline,
        &mut budget,
        &mut probed_batch,
        (&card_screen(), &fence(token, 100, 100, 5)),
    );
    assert_eq!(
        timeline.lock().unwrap().card_unseen(),
        None,
        "the card was seen"
    );
    let seen = seen_rows(&timeline);
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(
        seen[0].contains("row=101 ") && seen[0].contains(" from=5 "),
        "{seen:?}"
    );
    assert_eq!(
        budget,
        Some((token, CARD_PROBE_BATCHES - 1)),
        "one batch spent"
    );
}

/// Relaunched with no clear, the probe passes over the card the earlier run left and dates
/// the slice that draws the drawer's own card, at that card's row; on the alternate screen
/// it dates a card only once the drawer has cleared it.
#[test]
fn the_probe_passes_over_a_card_an_earlier_run_left_above_the_drawers_start() {
    let (timeline, token) = drawer_timeline();
    let (mut budget, mut probed_batch) = (None, false);
    let old = card_screen();
    super::note_card_seen_in(
        7,
        &timeline,
        &mut budget,
        &mut probed_batch,
        (&old, &fence(token, 110, 100, 4)),
    );
    assert!(
        seen_rows(&timeline).is_empty(),
        "the old card is not the drawer's"
    );
    assert_eq!(
        timeline.lock().unwrap().card_unseen(),
        Some((token, CardProgram::Claude))
    );
    let mut both = old.clone();
    both.truncate(11);
    both.extend(card_screen().into_iter().take(13));
    super::note_card_seen_in(
        7,
        &timeline,
        &mut budget,
        &mut probed_batch,
        (&both, &fence(token, 110, 100, 9)),
    );
    let seen = seen_rows(&timeline);
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(
        seen[0].contains("row=112 ") && seen[0].contains(" from=9 "),
        "the drawer's card, dated by the slice that drew it: {seen:?}"
    );

    let (timeline, token) = drawer_timeline();
    let (mut budget, mut probed_batch) = (None, false);
    let alt = |alt_fresh| {
        let mut f = fence(token, 110, 100, 4);
        f.alt = true;
        f.holder.alt_fresh = alt_fresh;
        f
    };
    super::note_card_seen_in(
        7,
        &timeline,
        &mut budget,
        &mut probed_batch,
        (&old, &alt(false)),
    );
    assert!(
        seen_rows(&timeline).is_empty(),
        "an alternate screen the drawer did not clear"
    );
    super::note_card_seen_in(
        7,
        &timeline,
        &mut budget,
        &mut probed_batch,
        (&old, &alt(true)),
    );
    assert_eq!(
        seen_rows(&timeline).len(),
        1,
        "the drawer's alternate screen"
    );
}

/// While the fence's holder is another token, the card at the span's start is that
/// holder's and is not dated; under the drawer's token the same rows are.
#[test]
fn the_probe_dates_no_card_under_another_holders_fence() {
    let (timeline, token) = drawer_timeline();
    let (mut budget, mut probed_batch) = (None, false);
    let screen = card_screen();
    super::note_card_seen_in(
        7,
        &timeline,
        &mut budget,
        &mut probed_batch,
        (&screen, &fence(token ^ 1, 100, 100, 4)),
    );
    assert!(seen_rows(&timeline).is_empty(), "another holder's fence");
    assert_eq!(
        timeline.lock().unwrap().card_unseen(),
        Some((token, CardProgram::Claude))
    );
    super::note_card_seen_in(
        7,
        &timeline,
        &mut budget,
        &mut probed_batch,
        (&screen, &fence(token, 100, 100, 5)),
    );
    let seen = seen_rows(&timeline);
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(
        seen[0].contains("row=101 ") && seen[0].contains(" from=5 "),
        "the drawer's fence: {seen:?}"
    );
}
