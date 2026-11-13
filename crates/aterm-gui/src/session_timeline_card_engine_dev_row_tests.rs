// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::path::PathBuf;
use std::sync::Arc;

use atpkg::image::{ImageBuild, ImageSource, ProgramTag};

use super::{SessionTimeline, claim_supervisor_as};
use crate::holder_image::{CardProgram, EdgeKind, HolderImage};

fn rows(tl: &SessionTimeline, kind: &str) -> usize {
    tl.since(None).filter(|e| e.kind == kind).count()
}

fn claude_304(pgid: i32) -> HolderImage {
    HolderImage::of(
        pgid,
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
    )
}

#[test]
fn holder_named_is_recorded_for_a_card_program_only() {
    let mut tl = SessionTimeline::default();
    tl.note_foreground_group(70);
    assert!(tl.set_program(70, Some("zsh".into())));
    assert_eq!(rows(&tl, "holder-named"), 0, "a shell is no card program");
    tl.note_foreground_group(71);
    assert!(tl.set_program(71, Some("claude".into())));
    assert_eq!(rows(&tl, "holder-named"), 1);
    assert!(!tl.set_program(72, Some("claude".into())), "not in front");
    assert_eq!(rows(&tl, "holder-named"), 1);
}

#[test]
fn card_seen_is_recorded_once_for_the_current_drawers_own_card() {
    let mut tl = SessionTimeline::default();
    let image = claude_304(80);
    let token = image.token.expect("tracked");
    assert_eq!(tl.card_unseen(), None, "no drawer yet");
    assert!(
        !tl.note_card_seen(token, 3, "2.1.304", 0, 0),
        "not published"
    );
    tl.offer_holder_image(Arc::new(image), EdgeKind::Group);
    assert_eq!(tl.card_unseen(), Some((token, CardProgram::Claude)));
    assert!(!tl.note_card_seen(token, 4, "2.1.304", 0, 0), "agy's card");
    assert!(
        !tl.note_card_seen(token, 3, "2.1.303", 0, 0),
        "another build"
    );
    assert!(
        !tl.note_card_seen(token ^ 1, 3, "2.1.304", 0, 0),
        "another token"
    );
    assert_eq!(rows(&tl, "card-seen"), 0);
    assert!(tl.note_card_seen(token, 3, "2.1.304", 0, 0));
    assert!(
        !tl.note_card_seen(token, 3, "2.1.304", 0, 0),
        "once per token"
    );
    assert_eq!(rows(&tl, "card-seen"), 1);
    assert_eq!(tl.card_unseen(), None, "the probe stops looking");
}

/// A pass counts toward `bare_frames` once its snapshot holds the slice the probe saw the
/// card in (its screen mutations past the slice's `from`); a pass already in flight does not.
#[test]
fn bare_frames_count_the_passes_whose_snapshot_shows_the_seen_card() {
    use crate::claude_card::{Bare, CardOutcome};
    fn decorated(tl: &SessionTimeline) -> Vec<String> {
        tl.since(None)
            .filter(|e| e.kind == "card-decorated")
            .map(|e| e.payload.to_string())
            .collect()
    }
    let mut tl = SessionTimeline::default();
    let image = claude_304(80);
    let token = image.token.expect("tracked");
    tl.offer_holder_image(Arc::new(image), EdgeKind::Group);
    tl.note_card_frame(CardOutcome::Bare(token, Bare::Fence, Some(0)), 1, 1);
    tl.note_card_frame(CardOutcome::Bare(token, Bare::Fence, Some(0)), 2, 2);
    tl.note_card_frame(
        CardOutcome::Decorated {
            token,
            row: 9,
            armed: Some("new"),
        },
        3,
        3,
    );
    let rows = decorated(&tl);
    assert!(rows[0].contains("bare_frames=0"), "{rows:?}");
    let image = claude_304(81);
    let next = image.token.expect("tracked");
    tl.offer_holder_image(Arc::new(image), EdgeKind::Group);
    tl.note_card_frame(CardOutcome::Bare(next, Bare::Fence, Some(9)), 4, 10);
    assert!(tl.note_card_seen(next, 3, "2.1.304", 20, 10));
    tl.note_card_frame(CardOutcome::Bare(next, Bare::NoCard, None), 5, 10);
    tl.note_card_frame(CardOutcome::Bare(next, Bare::Fence, Some(9)), 6, 11);
    tl.note_card_frame(CardOutcome::Bare(next, Bare::Fence, Some(9)), 7, 12);
    tl.note_card_frame(
        CardOutcome::Decorated {
            token: next,
            row: 20,
            armed: Some("new"),
        },
        8,
        12,
    );
    let rows = decorated(&tl);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(
        rows[1].contains("bare_frames=2"),
        "the passes whose snapshot holds the card's slice, not the one in flight: {rows:?}"
    );
    let seen: Vec<String> = tl
        .since(None)
        .filter(|e| e.kind == "card-seen")
        .map(|e| e.payload.to_string())
        .collect();
    assert!(seen[0].contains("row=20 after=4 from=10"), "{seen:?}");
}

#[test]
fn holder_claim_is_recorded_for_a_claim_taken_not_renewed() {
    let session = crate::stub_session(9_401);
    let now = crate::metrics::now_us();
    let claim = |at: u64| {
        claim_supervisor_as(
            &session.ctx,
            "harness",
            None,
            Some(at + 60_000_000),
            None,
            at,
            aterm_slog::Actor::Ctl,
        )
    };
    let count = || rows(&session.ctx.timeline.lock().unwrap(), "holder-claim");
    assert!(claim(now).is_ok(), "taken");
    assert_eq!(count(), 1);
    assert!(claim(now + 1).is_ok(), "renewed by its holder");
    assert_eq!(count(), 1, "a renewal is no new claim");
}
