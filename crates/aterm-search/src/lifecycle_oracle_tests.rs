// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Differential equivalence oracle for [`LifecycleSearchIndex`] — the Wave-4
//! Track-4A milestone-1 acceptance gate.
//!
//! Property tests drive bounded random event sequences over the WHOLE
//! lifecycle alphabet (`Append`/`Replace`/`EvictBelow`/`Reflow`/`Clear`/
//! `AltScreenSwitch`) into (a) the incremental lifecycle index and (b) a
//! model of the surviving rows. After EVERY event, a legacy [`SearchIndex`]
//! is rebuilt from scratch over the model's surviving rows — exactly what the
//! legacy `Terminal::build_search_index` path does — and a query battery
//! (short/trigram/case-folded/Unicode/absent, both case modes, both
//! directions) must produce **byte-identical match sets** on both backends.
//!
//! Flags carry the one documented divergence (module docs of
//! `lifecycle_index.rs`): the legacy rebuild FORGETS retention eviction
//! (`incomplete == false` after rebuild), while the lifecycle index reports
//! it honestly. The oracle pins that exactly: `incomplete` equals the model's
//! true eviction state and `lowest_retained_abs_row` equals the model's true
//! retention boundary.

use std::collections::BTreeMap;

use crate::index::{EVICTION_RETAIN_DEN, EVICTION_RETAIN_NUM, SearchIndex};
use crate::lifecycle_driver::SearchLifecycleDriver;
use crate::lifecycle_index::{
    LifecycleSearchIndex, LifecycleSearchResults, SearchLifecycleEvent, UpsertOutcome,
};
use crate::types::{SearchDirection, SearchResults};

/// Deterministic 64-bit LCG (no external RNG dependency).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 16
    }

    fn below(&mut self, bound: u64) -> u64 {
        if bound == 0 { 0 } else { self.next() % bound }
    }
}

/// Line vocabulary chosen to exercise every match path: trigram hits,
/// short-query scans, ASCII and Unicode case folding, empty lines, and
/// highly repetitive text.
const VOCAB: &[&str] = &[
    "needle in the flood",
    "NEEDLE ALL CAPS",
    "warm läke shore",
    "plain filler text",
    "x0 x1 x2 x3",
    "flood flood flood flood",
    "",
    "tail Ä π unicode",
    "prefix needle suffix",
    "0 numeric 0",
    "short",
    "e",
];

/// Query battery: empty, short (1–2 byte), trigram, case-folded, Unicode,
/// and definitely-absent terms.
const QUERIES: &[&str] = &[
    "", "e", "0", "ne", "needle", "NEEDLE", "läke", "Ä", "flood", "zzq",
];

/// Regex query battery (4A P4): repetition, classes, anchors, alternation,
/// and a case-fold-sensitive class, run with `is_regex = true` in both case
/// modes and both directions.
#[cfg(feature = "regex")]
const REGEX_QUERIES: &[&str] = &["ne+dle", "x[0-9]", "flood$", "needle|läke", "[A-Z]{3}"];

/// One screen's surviving rows in the model (absolute row → text).
#[derive(Default, Clone)]
struct ModelScreen {
    rows: BTreeMap<u64, String>,
    lo: u64,
    hi: u64,
    evicted_any: bool,
    lowest_retained: u64,
    /// True once the mirrored cache cap really dropped rows (distinct from
    /// `evicted_any`, which is RETENTION eviction — the two carry different
    /// clear/reset semantics on both sides of the oracle).
    cap_evicted: bool,
    /// High-water mark of mirrored cap eviction, in absolute rows. Monotone:
    /// the incremental index reports the max of its per-epoch watermark and
    /// the re-epoch carry, which is exactly a monotone absolute watermark.
    cap_watermark: u64,
}

impl ModelScreen {
    fn evict_below(&mut self, oldest: u64) {
        if oldest <= self.lo {
            return;
        }
        let clamped = oldest.min(self.hi);
        self.rows.retain(|&abs, _| abs >= clamped);
        self.lo = clamped;
        self.evicted_any = true;
        self.lowest_retained = self.lowest_retained.max(clamped);
    }
}

/// Full oracle model mirroring the documented lifecycle semantics.
struct Model {
    main: ModelScreen,
    alt: ModelScreen,
    alt_active: bool,
    /// Cache cap mirrored from the index under test. The mirror replays the
    /// [`SearchIndex`] hysteresis (drop to `NUM/DEN · cap` oldest-first once
    /// the cap is exceeded) so capped oracles have a ground truth when cap
    /// eviction interleaves with retention and re-epochs. `usize::MAX` is
    /// effectively uncapped (every pre-existing oracle configuration).
    cap: usize,
}

impl Default for Model {
    fn default() -> Self {
        Self::with_cap(usize::MAX)
    }
}

impl Model {
    fn with_cap(cap: usize) -> Self {
        Self {
            main: ModelScreen::default(),
            alt: ModelScreen::default(),
            alt_active: false,
            cap,
        }
    }

    fn active(&self) -> &ModelScreen {
        if self.alt_active {
            &self.alt
        } else {
            &self.main
        }
    }

    fn active_mut(&mut self) -> &mut ModelScreen {
        if self.alt_active {
            &mut self.alt
        } else {
            &mut self.main
        }
    }

    fn apply(&mut self, event: &SearchLifecycleEvent) {
        match event {
            SearchLifecycleEvent::Append { abs_row, text }
            | SearchLifecycleEvent::Replace { abs_row, text } => {
                let cap = self.cap;
                let screen = self.active_mut();
                if screen.rows.is_empty() && screen.lo == screen.hi && *abs_row >= screen.lo {
                    screen.lo = *abs_row;
                    screen.hi = *abs_row;
                }
                if *abs_row < screen.lo {
                    return; // stale write: dropped by both sides
                }
                // Mirror of the incremental upsert's cap-watermark guard (4A
                // P1, both its index-local and re-epoch-carried forms): a
                // write below the cap eviction watermark is dropped, never
                // re-admitted.
                if screen.cap_evicted && *abs_row < screen.cap_watermark {
                    return;
                }
                screen.rows.insert(*abs_row, text.clone());
                screen.hi = screen.hi.max(abs_row + 1);
                // Mirror of `SearchIndex` cap eviction: once the cache
                // exceeds `cap`, drop oldest-first down to the `NUM/DEN`
                // low-water mark and advance the watermark to the smallest
                // survivor. Timing tracks the incremental index exactly
                // because both trigger on the same cached-line count.
                if screen.rows.len() > cap {
                    let target = cap.saturating_mul(EVICTION_RETAIN_NUM) / EVICTION_RETAIN_DEN;
                    while screen.rows.len() > target {
                        let oldest = *screen.rows.keys().next().expect("rows nonempty");
                        screen.rows.remove(&oldest);
                    }
                    screen.cap_evicted = true;
                    let lowest = screen.rows.keys().next().copied().unwrap_or(screen.hi);
                    screen.cap_watermark = screen.cap_watermark.max(lowest);
                }
            }
            SearchLifecycleEvent::EvictBelow { oldest_retained } => {
                self.active_mut().evict_below(*oldest_retained);
            }
            SearchLifecycleEvent::Reflow {
                first_retained,
                rows,
            } => {
                model_screen_reflow(self.active_mut(), *first_retained, rows);
            }
            SearchLifecycleEvent::Clear { next_row } => {
                let screen = self.active_mut();
                screen.rows.clear();
                screen.lo = *next_row;
                screen.hi = *next_row;
                screen.evicted_any = false;
                screen.lowest_retained = *next_row;
                // `SearchIndex::clear` resets the eviction watermark too: a
                // cleared index has not evicted anything.
                screen.cap_evicted = false;
                screen.cap_watermark = 0;
            }
            SearchLifecycleEvent::AltScreenSwitch { alt } => {
                if *alt && !self.alt_active {
                    self.alt = ModelScreen::default();
                }
                self.alt_active = *alt;
            }
        }
    }
}

/// Model reflow with the 4A P3 clamp semantics: a layout may never
/// resurrect evicted/cleared row space — rows below the retained window are
/// dropped, the rest keep their claimed numbering.
fn model_screen_reflow(screen: &mut ModelScreen, first_retained: u64, rows: &[String]) {
    let clamped_first = first_retained.max(screen.lo);
    let skip = usize::try_from(clamped_first - first_retained).expect("oracle rows stay small");
    let rows = rows.get(skip..).unwrap_or(&[]);
    screen.rows.clear();
    for (i, text) in rows.iter().enumerate() {
        screen.rows.insert(clamped_first + i as u64, text.clone());
    }
    screen.lo = clamped_first;
    screen.hi = clamped_first + rows.len() as u64;
}

/// Rebuild a legacy index from scratch over the model's surviving rows —
/// exactly the legacy `Terminal::build_search_index` shape (absolute-row
/// keys, ascending order, fresh index per content change).
fn legacy_rebuild(screen: &ModelScreen, cap: usize) -> SearchIndex {
    let mut index = SearchIndex::with_capacity_and_max(0, cap);
    for (&abs, text) in &screen.rows {
        index.index_line(usize::try_from(abs).expect("oracle rows stay small"), text);
    }
    index
}

fn legacy_matches(results: &SearchResults) -> Vec<(u64, usize, usize)> {
    results
        .matches
        .iter()
        .map(|m| (m.line as u64, m.start_col, m.end_col))
        .collect()
}

fn lifecycle_matches(results: &LifecycleSearchResults) -> Vec<(u64, usize, usize)> {
    results
        .matches
        .iter()
        .map(|m| (m.abs_row, m.start_col, m.end_col))
        .collect()
}

/// Compare the full query battery (literal + regex per 4A P4) on both
/// backends after one event, via any lifecycle-shaped search entry point.
fn assert_battery_against(
    search: &dyn Fn(&str, bool, bool, SearchDirection) -> LifecycleSearchResults,
    model: &Model,
    cap: usize,
    check_flags: bool,
    context: &str,
) {
    let screen = model.active();
    let reference = legacy_rebuild(screen, cap);
    let check = |query: &str, is_regex: bool| {
        for case_sensitive in [false, true] {
            for direction in [SearchDirection::Forward, SearchDirection::Backward] {
                let expected = reference
                    .search_results_opts_direction(query, case_sensitive, is_regex, direction)
                    .expect("legacy search");
                let actual = search(query, case_sensitive, is_regex, direction);
                assert_eq!(
                    lifecycle_matches(&actual),
                    legacy_matches(&expected),
                    "{context}: query {query:?} regex={is_regex} cs={case_sensitive} \
                     dir={direction:?}: matches must be byte-identical to the legacy rebuild"
                );
                if check_flags {
                    // Cap eviction and retention eviction both make results
                    // incomplete; the reported watermark is the max of the
                    // two boundaries (uncapped screens have cap fields at
                    // their defaults, so this reduces to the retention form).
                    assert_eq!(
                        actual.incomplete,
                        screen.evicted_any || screen.cap_evicted,
                        "{context}: query {query:?} regex={is_regex}: incomplete must \
                         equal the model's true eviction state"
                    );
                    let expected_lowest = if screen.evicted_any || screen.cap_evicted {
                        screen.lowest_retained.max(screen.cap_watermark)
                    } else {
                        0
                    };
                    assert_eq!(
                        actual.lowest_retained_abs_row, expected_lowest,
                        "{context}: query {query:?} regex={is_regex}: lowest retained \
                         row must be the model's true retention boundary"
                    );
                }
            }
        }
    };
    for query in QUERIES {
        check(query, false);
    }
    #[cfg(feature = "regex")]
    for query in REGEX_QUERIES {
        check(query, true);
    }
}

/// Compare the full query battery on both backends after one event.
fn assert_battery_equivalent(
    idx: &LifecycleSearchIndex,
    model: &Model,
    cap: usize,
    check_flags: bool,
    context: &str,
) {
    assert_battery_against(
        &|query, case_sensitive, is_regex, direction| {
            idx.search_results_opts_direction(query, case_sensitive, is_regex, direction)
                .expect("lifecycle search")
        },
        model,
        cap,
        check_flags,
        context,
    );
}

fn random_text(rng: &mut Lcg) -> String {
    VOCAB[rng.below(VOCAB.len() as u64) as usize].to_string()
}

/// Generate the next event over the full alphabet for the active screen.
fn random_event(rng: &mut Lcg, model: &Model) -> SearchLifecycleEvent {
    let screen = model.active();
    let span = screen.hi.saturating_sub(screen.lo);
    match rng.below(100) {
        // Appends dominate, as on a real PTY stream.
        0..=54 => SearchLifecycleEvent::Append {
            abs_row: screen.hi,
            text: random_text(rng),
        },
        55..=74 if span > 0 => SearchLifecycleEvent::Replace {
            abs_row: screen.lo + rng.below(span),
            text: random_text(rng),
        },
        75..=82 if span > 0 => SearchLifecycleEvent::EvictBelow {
            oldest_retained: screen.lo + 1 + rng.below(span),
        },
        83..=89 => {
            let count = rng.below(12) as usize;
            // Sometimes out-of-contract (below the retained window) so the
            // P3 clamp is pinned on both sides of the oracle.
            SearchLifecycleEvent::Reflow {
                first_retained: screen.lo.saturating_sub(rng.below(3)),
                rows: (0..count).map(|_| random_text(rng)).collect(),
            }
        }
        90..=93 => SearchLifecycleEvent::Clear {
            next_row: screen.hi,
        },
        94..=99 => SearchLifecycleEvent::AltScreenSwitch {
            alt: !model.alt_active,
        },
        _ => SearchLifecycleEvent::Append {
            abs_row: screen.hi,
            text: random_text(rng),
        },
    }
}

/// THE milestone gate: bounded random event sequences over the whole
/// alphabet; after every event, matches are byte-identical to a legacy
/// from-scratch rebuild, and the flags carry the honest eviction state.
#[test]
fn oracle_full_alphabet_matches_are_byte_identical_to_legacy_rebuild() {
    const CAP: usize = 100_000; // production default: no internal cap eviction
    for seed in 0..32u64 {
        let mut rng = Lcg(0x9E3779B97F4A7C15 ^ (seed.wrapping_mul(0xA76BB1)));
        let mut idx = LifecycleSearchIndex::new();
        let mut model = Model::default();
        for step in 0..48 {
            let event = random_event(&mut rng, &model);
            idx.apply(&event);
            model.apply(&event);
            assert_battery_equivalent(
                &idx,
                &model,
                CAP,
                true,
                &format!("seed {seed} step {step} event {event:?}"),
            );
        }
    }
}

/// Event generator for the id-exhaustion oracle: append-dominated with
/// replaces, evictions, and rare clears. Deliberately excludes `Reflow` and
/// `AltScreenSwitch` — both rebase the epoch wholesale, which would let the
/// sequence dodge the offset-overflow paths this variant exists to exercise
/// (the first oracle already covers the full alphabet).
fn random_event_exhaustion(rng: &mut Lcg, model: &Model) -> SearchLifecycleEvent {
    let screen = model.active();
    let span = screen.hi.saturating_sub(screen.lo);
    match rng.below(100) {
        0..=11 if span > 0 => SearchLifecycleEvent::Replace {
            abs_row: screen.lo + rng.below(span),
            text: random_text(rng),
        },
        12..=27 if span > 0 => SearchLifecycleEvent::EvictBelow {
            oldest_retained: screen.lo + 1 + rng.below(span),
        },
        28..=29 => SearchLifecycleEvent::Clear {
            next_row: screen.hi,
        },
        _ => SearchLifecycleEvent::Append {
            abs_row: screen.hi,
            text: random_text(rng),
        },
    }
}

/// Outcome telemetry from one exhaustion-oracle run, so each caller can
/// assert the paths its configuration exists to exercise really fired.
#[derive(Default)]
struct ExhaustionStats {
    reepoched: bool,
    forced: bool,
    cap_evicted: bool,
    watermark_drops: usize,
}

/// The exhaustion-oracle body, parameterized by the cache cap: bounded
/// random append/replace/evict/clear sequences under a TINY id ceiling, so
/// re-epoching and forced id-exhaustion retention cuts interleave with the
/// event stream — and, under a small cap, with internal cache-cap eviction.
/// Forced cuts are mirrored into the model (they ARE retention eviction), so
/// match byte-identity and flag honesty must still hold on every step.
fn run_exhaustion_oracle(cap: usize, seeds: std::ops::Range<u64>) -> ExhaustionStats {
    // Reference-rebuild cap: the model's rows already reflect the mirrored
    // cache cap, so the reference pass must never re-evict them.
    const REBUILD_CAP: usize = 100_000;
    const CEILING: u32 = 24;
    let mut stats = ExhaustionStats::default();
    for seed in seeds {
        let mut rng = Lcg(0xD1B54A32D192ED03 ^ (seed.wrapping_mul(0x9FB21C651E98DF25)));
        let mut idx = LifecycleSearchIndex::with_max_cached_and_ceiling(cap, CEILING);
        let mut model = Model::with_cap(cap);
        for step in 0..96 {
            let event = random_event_exhaustion(&mut rng, &model);
            let window_lo = idx.retained_row_window().0;
            let outcome = idx.apply(&event);
            model.apply(&event);
            match outcome {
                UpsertOutcome::Reepoched => stats.reepoched = true,
                UpsertOutcome::ForcedEviction => {
                    stats.forced = true;
                    // The forced cut retains the newest CEILING+1 rows ending
                    // at the event's row — mirror it as model retention.
                    if let SearchLifecycleEvent::Append { abs_row, .. }
                    | SearchLifecycleEvent::Replace { abs_row, .. } = &event
                    {
                        model
                            .active_mut()
                            .evict_below(abs_row.saturating_sub(u64::from(CEILING)));
                    }
                }
                UpsertOutcome::StaleDropped => {
                    // A drop at/above the retained window is the cap-watermark
                    // upsert guard firing (index-local or re-epoch-carried).
                    if let SearchLifecycleEvent::Append { abs_row, .. }
                    | SearchLifecycleEvent::Replace { abs_row, .. } = &event
                        && *abs_row >= window_lo
                    {
                        stats.watermark_drops += 1;
                    }
                }
                UpsertOutcome::Applied => {}
            }
            stats.cap_evicted |= model.active().cap_evicted;
            assert_battery_equivalent(
                &idx,
                &model,
                REBUILD_CAP,
                true,
                &format!("cap {cap} seed {seed} step {step} event {event:?}"),
            );
        }
    }
    stats
}

/// The exhaustion oracle at the production-shaped cap (no internal cache
/// eviction): re-epochs and forced cuts interleave with retention only.
#[test]
fn oracle_full_alphabet_holds_across_reepoch_and_id_exhaustion() {
    let stats = run_exhaustion_oracle(100_000, 100..124);
    assert!(
        stats.reepoched,
        "the tiny ceiling must exercise re-epoching"
    );
    assert!(stats.forced, "the tiny ceiling must exercise forced cuts");
}

/// The audit's coverage gap: no oracle interleaved internal cache-cap
/// eviction with re-epochs — exactly where the carried-watermark upsert
/// guard (4A P1×P8) is all that stands between a stale Replace and
/// re-admitting an evicted row (the fresh post-re-epoch index resets its
/// local eviction flags). A small cap keeps cap eviction constant while the
/// tiny ceiling keeps re-epoching; the capped model mirror is the ground
/// truth for both at once.
#[test]
fn oracle_small_cap_interleaves_cap_eviction_with_reepoch() {
    let stats = run_exhaustion_oracle(6, 400..424);
    assert!(
        stats.reepoched,
        "the small-cap run must exercise re-epoching"
    );
    assert!(
        stats.cap_evicted,
        "the small cap must exercise cap eviction"
    );
    assert!(
        stats.watermark_drops > 0,
        "the sequences must exercise replaces dropped below the cap watermark"
    );
}

/// Internal cache-cap eviction parity: for an append/replace stream (the
/// live-terminal shape) under a SMALL cache cap, the incremental index and
/// the legacy from-scratch rebuild evict identically — full equality
/// including the incomplete flag and watermark, both mapped to absolute rows.
#[test]
fn oracle_cache_cap_eviction_is_identical_for_append_streams() {
    const CAP: usize = 32;
    for seed in 200..216u64 {
        let mut rng = Lcg(0xBF58476D1CE4E5B9 ^ seed);
        let mut idx = LifecycleSearchIndex::with_max_cached_lines(CAP);
        let mut model = Model::default();
        for step in 0..96 {
            let screen_hi = model.active().hi;
            // Appends plus replaces confined to the newest CAP/2 rows (never
            // below the incremental watermark, where a replace would
            // legitimately re-admit an evicted row and diverge from a
            // single-pass rebuild — documented in lifecycle_index.rs).
            let event = if rng.below(4) == 0 && screen_hi > 0 {
                let newest = CAP as u64 / 2;
                let lo = screen_hi.saturating_sub(newest).max(model.active().lo);
                SearchLifecycleEvent::Replace {
                    abs_row: lo + rng.below(screen_hi - lo),
                    text: random_text(&mut rng),
                }
            } else {
                SearchLifecycleEvent::Append {
                    abs_row: screen_hi,
                    text: random_text(&mut rng),
                }
            };
            idx.apply(&event);
            model.apply(&event);

            // Reference: single-pass rebuild with the SAME small cap over the
            // full append history (the model never drops rows here).
            let reference = legacy_rebuild(model.active(), CAP);
            for query in QUERIES {
                let expected = reference
                    .search_results_opts(query, false, false)
                    .expect("legacy search");
                let actual = idx
                    .search_results_opts(query, false, false)
                    .expect("lifecycle search");
                assert_eq!(
                    lifecycle_matches(&actual),
                    legacy_matches(&expected),
                    "seed {seed} step {step} query {query:?}: cap-eviction match parity"
                );
                assert_eq!(
                    actual.incomplete, expected.incomplete,
                    "seed {seed} step {step} query {query:?}: cap-eviction flag parity"
                );
                assert_eq!(
                    actual.lowest_retained_abs_row, expected.lowest_retained_line as u64,
                    "seed {seed} step {step} query {query:?}: cap-eviction watermark parity"
                );
            }
        }
    }
}

/// 4A P2: the two-screen DRIVER oracle. Random event sequences interleave
/// active-screen content, MAIN-screen retention (eviction/reflow — deferred
/// while alt is active, replayed on exit), clears, and alt toggles. After
/// every event the ACTIVE screen's full battery must be byte-identical to a
/// legacy rebuild of the model — the model applies main-screen retention
/// IMMEDIATELY, so equality across alt exits proves defer+replay changes
/// nothing observable, on either screen.
#[test]
fn oracle_two_screen_driver_defers_and_replays_main_retention() {
    const CAP: usize = 100_000;
    let mut replayed_any = false;
    for seed in 300..324u64 {
        let mut rng = Lcg(0x2545F4914F6CDD1D ^ (seed.wrapping_mul(0x9E3779B9)));
        let mut driver = SearchLifecycleDriver::new();
        let mut model = Model::default();
        for step in 0..64 {
            let screen = model.active();
            let span = screen.hi.saturating_sub(screen.lo);
            let roll = rng.below(100);
            let context = match roll {
                0..=44 => {
                    let abs_row = screen.hi;
                    let text = random_text(&mut rng);
                    driver.append(abs_row, &text);
                    model.apply(&SearchLifecycleEvent::Append { abs_row, text });
                    format!("append {abs_row}")
                }
                45..=59 if span > 0 => {
                    let abs_row = screen.lo + rng.below(span);
                    let text = random_text(&mut rng);
                    driver.replace(abs_row, &text);
                    model.apply(&SearchLifecycleEvent::Replace { abs_row, text });
                    format!("replace {abs_row}")
                }
                60..=71 => {
                    // MAIN-screen retention eviction, regardless of the
                    // active screen (deferred by the driver under alt).
                    let main_span = model.main.hi.saturating_sub(model.main.lo);
                    if main_span == 0 {
                        continue;
                    }
                    let oldest = model.main.lo + 1 + rng.below(main_span);
                    driver.main_evict_below(oldest);
                    model.main.evict_below(oldest);
                    format!("main_evict {oldest} (alt={})", model.alt_active)
                }
                72..=81 => {
                    // MAIN-screen reflow, sometimes out-of-contract (P3).
                    let count = rng.below(10) as usize;
                    let first = model.main.lo.saturating_sub(rng.below(2));
                    let rows: Vec<String> = (0..count).map(|_| random_text(&mut rng)).collect();
                    driver.main_reflow(first, rows.clone());
                    model_screen_reflow(&mut model.main, first, &rows);
                    format!("main_reflow {first} (alt={})", model.alt_active)
                }
                82..=87 => {
                    let next_row = screen.hi;
                    driver.clear(next_row);
                    model.apply(&SearchLifecycleEvent::Clear { next_row });
                    format!("clear {next_row}")
                }
                _ => {
                    let alt = !model.alt_active;
                    if !alt && driver.deferred_main_events() > 0 {
                        replayed_any = true;
                    }
                    driver.set_alt_screen(alt);
                    model.apply(&SearchLifecycleEvent::AltScreenSwitch { alt });
                    format!("alt={alt}")
                }
            };
            assert_battery_against(
                &|query, case_sensitive, is_regex, direction| {
                    driver
                        .search_results_opts_direction(query, case_sensitive, is_regex, direction)
                        .expect("driver search")
                },
                &model,
                CAP,
                true,
                &format!("seed {seed} step {step} {context}"),
            );
        }
    }
    assert!(
        replayed_any,
        "the sequences must exercise at least one deferred-main replay"
    );
}
