// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::animal_glyphs_gen::AnimalGlyphId;
use crate::cat_baker::CatColorKey;
use crate::genome::{NovaMagic, nova_features};
use crate::spec::{BurstKind, Collection};
use crate::state_words::{StateWords, Word};
use aterm_lexicon::{Class, FormId, LangSet};
use aterm_time::{Duration, Instant};

fn words(d: &WordDecorations, t0: Instant) -> Vec<Word> {
    let mut w = StateWords::new(t0);
    d.state_words(&mut w);
    w.into_words()
}

#[test]
fn a_hashed_word_field_moves_the_word_words() {
    let t0 = Instant::now();
    let genome = crate::genome::Genome { gkey: 3, magic: 4 };
    let fresh = || {
        let mut d = WordDecorations::default();
        d.persist.insert(
            11,
            Episode::fresh(t0 + Duration::from_millis(7), genome, 5, 2, 1),
        );
        d.done_marks.insert(21);
        d.done_marks.insert(22);
        d
    };
    let base = words(&fresh(), t0);
    assert_eq!(base, words(&fresh(), t0));

    let mut done = fresh();
    if let Some(ep) = done.persist.get_mut(&11) {
        ep.nova_done = true;
    }
    assert_ne!(words(&done, t0), base, "an episode's done latch is hashed");
    let mut touched = fresh();
    touched.done_marks.touch(21);
    assert_ne!(words(&touched, t0), base, "the done marks' order is hashed");
    let mut marked = fresh();
    marked.done_marks.insert(23);
    assert_ne!(words(&marked, t0), base, "a new done mark is hashed");
    let mut away = fresh();
    away.away = true;
    assert_ne!(
        words(&away, t0),
        base,
        "the word pass's clocks and latches are hashed"
    );
    let mut scanned = fresh();
    scanned.scan_text.push_str("scratch");
    assert_eq!(words(&scanned, t0), base, "the scan's scratch is not state");
}

/// A named change to a fresh word pass, given the script's `t0`.
type Case = (&'static str, fn(&mut WordDecorations, Instant));

#[test]
fn a_kept_reservation_witness_flash_and_birth_moves_the_word_words() {
    let t0 = Instant::now();
    let base = words(&WordDecorations::default(), t0);
    let cases: [Case; 7] = [
        ("an ignition reservation", |d, t0| {
            d.ignitions.push(IgnitionReservation {
                pane: Some(9),
                owner: 11,
                start: t0 + Duration::from_millis(3),
                center: (40, 18),
            });
        }),
        ("the typed-edit witness", |d, t0| {
            d.note_typed_edit(t0 + Duration::from_millis(2), None);
        }),
        ("a noted trick word", |d, t0| {
            d.trick_flash.note(t0, None, None, Some((2, 5)), "sit", 0);
        }),
        ("a pending birth", |d, _| {
            d.pending.push(PendingBirth {
                occ_idx: 0,
                ctx_fp: 5,
                live_caret_completion: false,
                at_live_cursor: true,
            });
        }),
        ("the cursor kitty's last sprite", |d, _| {
            d.kitty_cursor_placement.last_sprite = Some((30, 12));
        }),
        ("a word's taken ink", |d, _| {
            d.ink_base_fg.push([200, 40, 10]);
            d.ink_cols.push(4);
        }),
        ("the pet's ink span", |d, _| d.pet_ink_spans.push((2, 9))),
    ];
    for (what, set) in cases {
        let mut d = WordDecorations::default();
        set(&mut d, t0);
        assert_ne!(words(&d, t0), base, "{what} is hashed");
    }

    let reserved_in = |pane| {
        let mut d = WordDecorations::default();
        d.ignitions.push(IgnitionReservation {
            pane: Some(pane),
            owner: 11,
            start: t0,
            center: (40, 18),
        });
        words(&d, t0)
    };
    assert_eq!(
        reserved_in(9),
        reserved_in(14),
        "a pane's id is the process's count of terminals, not state"
    );
    let hosted = WordDecorations {
        pane_px: (120, 40),
        grace_override: Some(Duration::ZERO),
        ..WordDecorations::default()
    };
    assert_eq!(
        words(&hosted, t0),
        base,
        "the host's pane origin and a test's grace override are settings, not state"
    );
}

/// A named change to an occurrence.
type OccurrenceCase = (&'static str, fn(&mut Occurrence));

fn occurrence(t0: Instant) -> Occurrence {
    Occurrence {
        row: 1,
        start_col: 2,
        end_col: 6,
        class: Class::Profanity,
        langs: LangSet::EMPTY,
        form_id: FormId::UNKNOWN,
        seed: 5,
        ident: 9,
        appeared: t0,
        genome: crate::genome::Genome { gkey: 3, magic: 4 },
        ink_base: 0,
        ink_cells: 0,
        ink_bg: [0; 3],
        cat_colors: CatColorKey::default(),
        cat_text_clear: false,
        cat_peek_down: false,
        dec_line: false,
        inert: false,
        spec: WordEffectSpec::NONE,
        custom: false,
        species: None,
    }
}

fn nova(t0: Instant) -> NovaLive {
    NovaLive {
        idx: 0,
        start: t0,
        center_px: (40, 18),
        r_max: 20.0,
        feats: nova_features(0),
        magic: None,
    }
}

#[test]
fn an_occurrences_class_languages_ground_spec_and_species_and_a_novas_features_are_hashed() {
    let t0 = Instant::now();
    let with = |o: Occurrence, n: NovaLive| {
        let mut d = WordDecorations::default();
        d.occ.push(o);
        d.novas.push(n);
        words(&d, t0)
    };
    let base = with(occurrence(t0), nova(t0));
    let cases: [OccurrenceCase; 7] = [
        ("its class", |o| o.class = Class::Feline),
        ("its languages", |o| o.langs = LangSet::from_bits(1)),
        ("the ground its ink was taken on", |o| {
            o.ink_bg = [10, 20, 30]
        }),
        ("its graphic", |o| {
            o.spec.graphic = Some(GraphicSpec {
                collection: Collection::Cats,
                dwell_ms: (400, 900),
            });
        }),
        ("its ink", |o| o.spec.ink = Some(crate::spec::rainbow_ink())),
        ("its burst", |o| {
            o.spec.burst = Some(BurstSpec {
                kind: BurstKind::Nova,
                chance_pct: 30,
            });
        }),
        ("its species", |o| o.species = Some(AnimalGlyphId::Bat)),
    ];
    for (what, set) in cases {
        let mut o = occurrence(t0);
        set(&mut o);
        assert_ne!(with(o, nova(t0)), base, "an occurrence's {what} is hashed");
    }
    assert_ne!(
        nova_features(0),
        nova_features(u64::MAX),
        "fixture: two genomes decode two novas"
    );
    let featured = NovaLive {
        feats: nova_features(u64::MAX),
        ..nova(t0)
    };
    assert_ne!(
        with(occurrence(t0), featured),
        base,
        "a nova's features are hashed"
    );
    let rare = NovaLive {
        magic: Some(NovaMagic::Quasar),
        ..nova(t0)
    };
    assert_ne!(
        with(occurrence(t0), rare),
        base,
        "a nova's rare variant is hashed"
    );
}
