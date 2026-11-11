// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::model::Decision;

/// Bytes, words and strings (each with its 0 ending) reach [`Fnv`] as `aterm_hash` folds them.
#[test]
fn the_band_hasher_folds_as_aterm_hash_does() {
    let mut fnv = Fnv::new();
    assert_eq!(fnv.0, aterm_hash::FNV64_OFFSET);
    fnv.byte(0x7f);
    let mut want = aterm_hash::fnv1a_64(&[0x7f]);
    assert_eq!(fnv.0, want);
    for word in [0, 1, u64::MAX, 0x0102_0304_0506_0708] {
        fnv.num(word);
        want = aterm_hash::fnv1a_64_u64_le(want, word);
        assert_eq!(fnv.0, want, "{word:#x}");
    }
    fnv.str("é a");
    want = aterm_hash::fnv1a_64_extend(want, "é a\0".as_bytes());
    assert_eq!(fnv.0, want);
}

/// Paint a layout's words into exactly `cols` cells the way the host will.
pub(crate) fn render(row: &RowLayout, cols: usize) -> String {
    let mut cells = vec![' '; cols];
    let mut put = |col: usize, s: &str| {
        for (i, ch) in s.chars().enumerate() {
            if let Some(cell) = cells.get_mut(col + i) {
                *cell = ch;
            }
        }
    };
    put(row.glyph.0, &row.glyph.1.to_string());
    put(row.title.0, &row.title.1);
    if let Some((col, d)) = &row.detail {
        put(col - 2, "\u{00b7}");
        put(*col, d);
    }
    if let Some((col, p)) = &row.pct {
        put(*col, p);
    }
    if let Some((col, words)) = row.load {
        put(col, words);
    }
    if let Some((col, s)) = &row.stats {
        put(*col, s);
    }
    for c in &row.capsules {
        put(c.col, &format!(" {} ", c.text));
    }
    cells.into_iter().collect()
}

pub(crate) fn chars(s: &str) -> usize {
    s.chars().count()
}

/// Whether `shown` is `words` whole, or cut only after a whole word of them.
pub(crate) fn whole_words_of(shown: &str, words: &str) -> bool {
    if shown == words {
        return true;
    }
    let Some(stem) = shown.strip_suffix('\u{2026}') else {
        return false;
    };
    !stem.is_empty()
        && aterm_grapheme::word_breaks(words)
            .iter()
            .any(|brk| words[..brk.at].trim_end_matches(TITLE_CUT_TRAIL) == stem)
}

/// Whether `row`'s title took the second clear cell before its capsules because a whole word needed it.
pub(crate) fn spent_clear_cell(
    spec: &RowSpec<'_>,
    row: &RowLayout,
    width: &dyn Fn(&str) -> usize,
) -> bool {
    let shown = &row.title.1;
    let cells = width(shown);
    let words = |text: &str| text.trim_end_matches('\u{2026}').split_whitespace().count();
    cells > 0
        && title_in(spec.title, cells, width).as_deref() == Some(shown.as_str())
        && title_in(spec.title, cells - 1, width).is_none_or(|fewer| words(&fewer) < words(shown))
}

fn id(n: u64) -> MessageId {
    MessageId::from_raw(n).unwrap()
}

fn caps(intents: &[Intent]) -> Vec<CapsuleSpec> {
    let mut v: Vec<CapsuleSpec> = intents
        .iter()
        .enumerate()
        .map(|(i, it)| CapsuleSpec::authored(it, i as u8))
        .collect();
    v.push(CapsuleSpec::details(0));
    v
}

pub(crate) const CRASH_DETAIL: &str =
    "crash log at ~/Library/Logs/aterm/crash-signal-8123-1758470000.log.seen";
pub(crate) const STAGED_MANUAL: &str = "build 1234 \u{2014} verified; auto-apply is off \u{2014} apply it from the Version menu or Software Update (in place; your shells keep running)";
pub(crate) const STAGED_AUTO: &str = "build 1234 \u{2014} verified; applies by itself at the next quiet moment, and within 15 min regardless \u{2014} nothing to do; your shells keep running";

/// The five width-law fixtures: crash, file access, staged (manual and auto), config warning, toolchain meter.
pub(crate) fn fixtures() -> Vec<(&'static str, RowSpec<'static>)> {
    vec![
        (
            "crash",
            RowSpec {
                kind: RowKind::Message(id(1)),
                severity: Severity::Error,
                live: false,
                glyph: '\u{26a0}',
                title: "aterm closed unexpectedly last time",
                detail0: Some(CRASH_DETAIL),
                step: false,
                meter: None,
                animated: false,
                busy: false,
                eta: false,
                load: None,
                load_slot: Loads::NONE,
                capsules: caps(&[Intent::OpenPath { path: "/x".into() }]),
            },
        ),
        (
            "file-access",
            RowSpec {
                kind: RowKind::Message(id(2)),
                severity: Severity::Info,
                live: false,
                glyph: '\u{2139}',
                title: "File access not confirmed",
                detail0: Some("Full Disk Access may already be enabled"),
                step: false,
                meter: None,
                animated: false,
                busy: false,
                eta: false,
                load: None,
                load_slot: Loads::NONE,
                capsules: caps(&[
                    Intent::OpenSystemPane {
                        pane: "full-disk-access".into(),
                    },
                    Intent::NotNow {
                        decision: Decision::FileAccess,
                    },
                ]),
            },
        ),
        (
            "staged-manual",
            RowSpec {
                kind: RowKind::Message(id(3)),
                severity: Severity::Success,
                live: false,
                glyph: '\u{2713}',
                title: "aterm v0.91.0 is ready",
                detail0: Some(STAGED_MANUAL),
                step: false,
                meter: None,
                animated: false,
                busy: false,
                eta: false,
                load: None,
                load_slot: Loads::NONE,
                capsules: caps(&[Intent::ApplyUpdate { build: 1234 }]),
            },
        ),
        (
            "staged-auto",
            RowSpec {
                kind: RowKind::Message(id(4)),
                severity: Severity::Success,
                live: false,
                glyph: '\u{2713}',
                title: "aterm v0.91.0 is ready",
                detail0: Some(STAGED_AUTO),
                step: false,
                meter: None,
                animated: false,
                busy: false,
                eta: false,
                load: None,
                load_slot: Loads::NONE,
                capsules: caps(&[]),
            },
        ),
        (
            "config",
            RowSpec {
                kind: RowKind::Message(id(5)),
                severity: Severity::Warn,
                live: false,
                glyph: '\u{26a0}',
                title: "3 keybindings in aterm.toml were skipped",
                detail0: Some("skipping \"cmd+shift+k\": unknown action \"foo\""),
                step: false,
                meter: None,
                animated: false,
                busy: false,
                eta: false,
                load: None,
                load_slot: Loads::NONE,
                capsules: caps(&[Intent::OpenConfigEditor { line: None }]),
            },
        ),
        (
            "toolchain",
            RowSpec {
                kind: RowKind::Message(id(6)),
                severity: Severity::Info,
                live: true,
                glyph: '\u{21e3}',
                title: "Installing ALab tools",
                detail0: Some("trust \u{00b7} extracting"),
                step: false,
                meter: Some((Some(420), "512 MB / 1.2 GB")),
                animated: true,
                busy: false,
                eta: false,
                load: None,
                load_slot: Loads::NONE,
                capsules: caps(&[Intent::OpenSettings {
                    route: "/packages".into(),
                }]),
            },
        ),
    ]
}

/// The live indicator's fixtures.
pub(crate) fn motion_fixtures() -> Vec<(&'static str, RowSpec<'static>)> {
    vec![
        (
            "download",
            RowSpec {
                kind: RowKind::Message(id(7)),
                severity: Severity::Info,
                live: true,
                glyph: '\u{21bb}',
                title: "Downloading aterm v0.92.0",
                detail0: None,
                step: false,
                meter: Some((Some(420), "31 MB / 74 MB")),
                animated: true,
                busy: false,
                eta: true,
                load: None,
                load_slot: Loads::NONE,
                capsules: caps(&[]),
            },
        ),
        (
            "first-run",
            RowSpec {
                kind: RowKind::Message(id(8)),
                severity: Severity::Info,
                live: true,
                glyph: '\u{21e3}',
                title: "Installing ALab tools",
                detail0: None,
                step: false,
                meter: Some((Some(420), "3 of 10 programs")),
                animated: true,
                busy: false,
                eta: true,
                load: Some(Load::Disk),
                load_slot: Loads::ALL,
                capsules: caps(&[]),
            },
        ),
        (
            "first-run-lull",
            RowSpec {
                kind: RowKind::Message(id(12)),
                severity: Severity::Info,
                live: true,
                glyph: '\u{21e3}',
                title: "Installing ALab tools",
                detail0: None,
                step: false,
                meter: Some((Some(420), "3 of 10 programs")),
                animated: true,
                busy: false,
                eta: true,
                load: None,
                load_slot: Loads::ALL,
                capsules: caps(&[]),
            },
        ),
        (
            "action-excerpt",
            RowSpec {
                kind: RowKind::Message(id(9)),
                severity: Severity::Info,
                live: true,
                glyph: '\u{21e3}',
                title: "Installing Command Line Tools and Homebrew",
                detail0: Some("enter your password in the macOS dialog"),
                step: false,
                meter: Some((None, "")),
                animated: true,
                busy: true,
                eta: false,
                load: Some(Load::Disk),
                load_slot: Loads::ALL,
                capsules: caps(&[]),
            },
        ),
        (
            "busy-excerpt",
            RowSpec {
                kind: RowKind::Message(id(10)),
                severity: Severity::Info,
                live: true,
                glyph: '\u{2191}',
                title: "Finishing aterm v0.92.0",
                detail0: Some("keys typed now arrive in a moment"),
                step: false,
                meter: Some((None, "")),
                animated: true,
                busy: true,
                eta: false,
                load: None,
                load_slot: Loads::NONE,
                capsules: caps(&[]),
            },
        ),
        (
            "held-meter",
            RowSpec {
                kind: RowKind::Message(id(11)),
                severity: Severity::Warn,
                live: false,
                glyph: '\u{26a0}',
                title: "ALab tools",
                detail0: Some("trust \u{2014} extracting 120 MB / 900 MB"),
                step: false,
                meter: Some((Some(430), "3 of 10")),
                animated: false,
                busy: false,
                eta: false,
                load: None,
                load_slot: Loads::NONE,
                capsules: caps(&[Intent::OpenSettings {
                    route: "/packages".into(),
                }]),
            },
        ),
    ]
}

#[test]
fn width_law_degrades_in_order() {
    use crate::model::UpgradeWord::{NotToday, Now, Skip};
    let upgrade = |word| Intent::AgentUpgrade {
        tab: "s-1".into(),
        to: "2.1.283".into(),
        word,
    };
    let overdue = |name, title, detail0, capsules| {
        (
            name,
            RowSpec {
                kind: RowKind::Message(id(9)),
                severity: Severity::Warn,
                live: true,
                glyph: '\u{26a0}',
                title,
                detail0: Some(detail0),
                step: false,
                meter: None,
                animated: false,
                busy: false,
                eta: false,
                load: None,
                load_slot: Loads::NONE,
                capsules,
            },
        )
    };
    let one_tab = "Couldn't upgrade Claude in tab 1 yet";
    let overdue_rows = [
        overdue(
            "overdue-upgrade",
            one_tab,
            "behind for 7 h: its turn is still running",
            caps(&[upgrade(NotToday), upgrade(Skip)]),
        ),
        overdue(
            "overdue-upgrade-26h",
            one_tab,
            "behind for 26 h: its turn is still running",
            caps(&[upgrade(NotToday), upgrade(Skip)]),
        ),
        overdue(
            "overdue-tabs",
            "Couldn't upgrade Claude in 2 tabs yet",
            "tab 1 for 7 h \u{b7} tab 3 for 2 h",
            caps(&[upgrade(Now), upgrade(NotToday)]),
        ),
    ];
    let mut clause_seen = std::collections::BTreeSet::new();
    for (name, spec) in fixtures()
        .into_iter()
        .chain(motion_fixtures())
        .chain(overdue_rows)
    {
        let authored = spec
            .capsules
            .iter()
            .filter(|c| !c.action.is_details())
            .count();
        let short_of = |r: &RowLayout| {
            r.capsules.len() < spec.capsules.len()
                || r.capsules.iter().any(|c| c.text != c.full_label)
        };
        let mut prev: Option<RowLayout> = None;
        for cols in (8..=200).rev() {
            let row = layout_row(&spec, cols, &chars);
            let painted = render(&row, cols);
            assert_eq!(chars(&painted), cols, "{name}@{cols}");
            assert_eq!(
                row.capsules
                    .iter()
                    .filter(|c| !c.action.is_details())
                    .count(),
                authored,
                "{name}@{cols}: authored capsules are never dropped"
            );
            let mut spans: Vec<(usize, usize)> = vec![(row.glyph.0, row.glyph.0 + 1)];
            if !row.title.1.is_empty() {
                spans.push((row.title.0, row.title.0 + chars(&row.title.1)));
            }
            if let Some((c, d)) = &row.detail {
                spans.push((c - 3, c + chars(d)));
            }
            let fill = spec.meter.and_then(|(f, _)| f).map(|f| f.min(1000));
            assert_eq!(
                row.meter,
                fill.map(|f| (0, cols, f)),
                "{name}@{cols}: the meter is the whole row"
            );
            assert_eq!(
                row.track,
                (spec.busy && fill.is_none()).then_some((0, cols)),
                "{name}@{cols}: the track is the whole row"
            );
            if let Some((c, p)) = &row.pct {
                spans.push((c - 1, c + chars(p)));
            }
            if let Some(c) = row.elapsed {
                spans.push((c - 1, c + row.elapsed_width()));
            }
            if let Some(c) = row.eta {
                spans.push((c - 1, c + row.eta_width()));
            }
            if let Some((c, w)) = row.load_slot {
                spans.push((c - 2, c + w));
            }
            if let (Some((c, w)), Some((slot, sw))) = (row.load, row.load_slot) {
                assert!(
                    c + chars(w) == slot + sw && chars(w) <= sw,
                    "{name}@{cols}: the words sit right-aligned in their slot"
                );
            }
            if let Some((c, s)) = &row.stats {
                spans.push((c - 2, c + chars(s)));
            }
            let mut sorted = spans.clone();
            sorted.sort();
            for pair in sorted.windows(2) {
                assert!(
                    pair[0].1 <= pair[1].0 || row.title.1.is_empty(),
                    "{name}@{cols}: {pair:?} overlap: {painted:?}"
                );
            }
            let left_end = spans.iter().map(|s| s.1).max().unwrap();
            if let Some(first) = row.capsules.first()
                && first.col > 0
                && !row.title.1.is_empty()
            {
                assert!(first.col >= left_end, "{name}@{cols}: overlap: {painted:?}");
                assert!(
                    first.col >= left_end + BEFORE_CAPSULES
                        || first.col == left_end + 1 && spent_clear_cell(&spec, &row, &chars),
                    "{name}@{cols}: {painted:?}"
                );
                let last = row.capsules.last().unwrap();
                assert_eq!(
                    last.col + last.width,
                    cols - MARGIN,
                    "{name}@{cols}: right-aligned"
                );
            }
            let moving = spec.animated || spec.busy;
            let activity = row.pct.is_some() || row.elapsed.is_some();
            let action = moving && spec.detail0.is_some_and(|d| !d.is_empty());
            if moving && !activity {
                assert!(
                    row.detail.is_none()
                        && row.stats.is_none()
                        && row.eta.is_none()
                        && row.load.is_none()
                        && row.load_slot.is_none(),
                    "{name}@{cols}: an extra outlived the activity: {painted:?}"
                );
            }
            if moving && activity && action {
                assert!(
                    row.load_slot.is_none()
                        || row.detail.as_ref().map(|d| d.1.as_str()) == spec.detail0,
                    "{name}@{cols}: the load slot only beside the whole action excerpt: {painted:?}"
                );
                assert!(
                    row.detail.is_some() || short_of(&row),
                    "{name}@{cols}: the action excerpt went before the capsules' long forms: {painted:?}"
                );
            }
            let traded = spec.eta && row.eta_short && row.load_slot.is_none();
            if moving && activity && !action && !traded {
                assert_eq!(
                    row.load.is_some(),
                    spec.load.is_some(),
                    "{name}@{cols}: the load words are the activity's: {painted:?}"
                );
                assert_eq!(
                    row.load_slot.is_some(),
                    spec.load.is_some() || !spec.load_slot.is_empty(),
                    "{name}@{cols}: the load slot is the activity's: {painted:?}"
                );
            }
            if let Some(p) = &prev {
                let short_now = short_of(&row);
                let short_prev = short_of(p);
                assert!(
                    !(short_prev && !short_now) || row.capsules.len() != p.capsules.len(),
                    "{name}@{cols}: capsules never go long again as the row narrows"
                );
                let elided_prev = p.title.1 != spec.title;
                if elided_prev && authored > 0 {
                    assert!(
                        short_now,
                        "{name}@{cols}: the title elides only after the capsules went short"
                    );
                }
                for (was, is, what) in [
                    (p.stats.is_some(), row.stats.is_some(), "stats"),
                    (p.detail.is_some(), row.detail.is_some(), "the excerpt"),
                    (p.eta.is_some(), row.eta.is_some(), "the ETA"),
                    (p.load.is_some(), row.load.is_some(), "the load words"),
                    (
                        p.load_slot.is_some(),
                        row.load_slot.is_some(),
                        "the load slot",
                    ),
                    (p.pct.is_some(), row.pct.is_some(), "the percent"),
                    (
                        p.elapsed.is_some(),
                        row.elapsed.is_some(),
                        "the elapsed slot",
                    ),
                ] {
                    assert!(
                        was || !is,
                        "{name}@{cols}: {what} came back while narrowing"
                    );
                }
                if short_now {
                    assert!(
                        (row.detail.is_none() || action)
                            && row.stats.is_none()
                            && (row.eta.is_none() || traded)
                            && (row.load_slot.is_none() || !action),
                        "{name}@{cols}: an extra beside short capsules: {painted:?}"
                    );
                }
                let long_stats = spec.meter.map(|(_, s)| s);
                let is_short = |r: &RowLayout| {
                    r.stats
                        .as_ref()
                        .is_some_and(|(_, s)| Some(s.as_str()) != long_stats)
                };
                assert!(
                    !(is_short(&p) && row.stats.is_some() && !is_short(&row)),
                    "{name}@{cols}: the stats went long again while narrowing"
                );
                assert!(
                    !(p.eta_short && row.eta.is_some() && !row.eta_short),
                    "{name}@{cols}: the ETA went long again while narrowing"
                );
                if row.eta_short {
                    assert!(
                        (row.detail.is_none() || action) && row.stats.is_none(),
                        "{name}@{cols}: an extra beside a short ETA: {painted:?}"
                    );
                }
                if row.eta.is_none() && spec.eta && row.pct.is_some() {
                    assert!(
                        (row.detail.is_none() || action) && row.stats.is_none(),
                        "{name}@{cols}: the ETA went first: {painted:?}"
                    );
                }
                if p.detail.is_some() && row.stats.is_some() {
                    assert!(
                        p.stats.is_some(),
                        "{name}@{cols}: stats go before the excerpt"
                    );
                }
            }
            let clause = spec.detail0.and_then(crate::text::lead_clause);
            if let Some((dcol, d)) = &row.detail
                && clause.as_ref() == Some(d)
            {
                clause_seen.insert(name);
                assert!(
                    !d.contains('\u{2026}') && *dcol == row.title.0 + chars(&row.title.1) + 3,
                    "{name}@{cols}: {d:?}"
                );
                let room = row
                    .capsules
                    .first()
                    .and_then(|c| c.col.checked_sub(BEFORE_CAPSULES + *dcol - 3));
                assert!(
                    chars(d) < DETAIL_FLOOR || room.is_some_and(|r| r >= 3 + DETAIL_FLOOR),
                    "{name}@{cols}: a clause the floor's cut would carry: {d:?}"
                );
            } else if let Some((dcol, d)) = &row.detail
                && d != spec.detail0.unwrap()
                && row.meter.is_none()
                && row.pct.is_none()
                && let Some(first) = row.capsules.first()
                && first.col > 0
                && let title_end = row.title.0 + chars(&row.title.1)
                && let Some(room) = first.col.checked_sub(BEFORE_CAPSULES + title_end)
            {
                assert!(
                    room >= 3 + DETAIL_FLOOR,
                    "{name}@{cols}: shaped with room {room}: {d:?}"
                );
                assert!(
                    d.contains('\u{2026}') && *dcol == title_end + 3,
                    "{name}@{cols}: {d:?}"
                );
                assert!(
                    chars(d) + 3 <= room,
                    "{name}@{cols}: {d:?} does not fit its room"
                );
            }
            prev = Some(row);
        }
    }
    assert_eq!(
        clause_seen.into_iter().collect::<Vec<_>>(),
        [
            "config",
            "overdue-tabs",
            "overdue-upgrade",
            "overdue-upgrade-26h"
        ],
        "the control: each overdue row painted its lead clause somewhere"
    );
}

#[test]
fn hit_test_agrees_with_layout() {
    let fixtures = fixtures();
    for cols in 8..=160usize {
        let mut rows: Vec<RowLayout> = fixtures
            .iter()
            .map(|(_, s)| layout_row(s, cols, &chars))
            .collect();
        let words = overflow_words(3, Links::Painted, None);
        rows.push(layout_row(&overflow_spec(3, &words), cols, &chars));
        let p = Presentation { cols, rows };
        for (r, row) in p.rows.iter().enumerate() {
            let RowKind::Message(id) = row.kind else {
                for col in 0..cols {
                    assert_eq!(p.hit(r, col), Hit::Overflow);
                }
                continue;
            };
            for col in 0..cols {
                let want = match row
                    .capsules
                    .iter()
                    .find(|c| col >= c.col && col < c.col + c.width)
                {
                    Some(c) if c.action.is_details() => Hit::Details(id),
                    Some(c) => Hit::Capsule(id, c.action),
                    None => Hit::Body(id),
                };
                assert_eq!(p.hit(r, col), want, "row {r} col {col} @ {cols}");
            }
            assert_eq!(p.hit(r, cols), Hit::Nothing);
        }
        assert_eq!(p.hit(p.rows.len(), 0), Hit::Nothing);
    }
}

#[test]
fn fingerprint_is_zero_when_empty_and_quantizes_the_fill() {
    assert_eq!(
        Presentation {
            cols: 80,
            rows: vec![]
        }
        .fingerprint(),
        0
    );
    let spec = fixtures()
        .into_iter()
        .find(|(n, _)| *n == "toolchain")
        .unwrap()
        .1;
    let at = |fill: u16, stats: &'static str| {
        let mut s = spec.clone();
        s.meter = Some((Some(fill), stats));
        Presentation {
            cols: 160,
            rows: vec![layout_row(&s, 160, &chars)],
        }
        .fingerprint()
    };
    let base = at(420, "512 MB / 1.2 GB");
    assert_eq!(base & 1, 1);
    for fill in 420..=429 {
        assert_eq!(
            base,
            at(fill, "512 MB / 1.2 GB"),
            "every permille inside 42% has the same key"
        );
    }
    for (fill, stats) in [
        (430, "512 MB / 1.2 GB"),
        (420, "513 MB / 1.2 GB"),
        (420, "513 MB / 1.3 GB"),
    ] {
        let fp = at(fill, stats);
        assert_eq!(fp & 1, 1);
        assert_eq!(fp, at(fill, stats));
    }
    let other_cols = Presentation {
        cols: 120,
        rows: vec![layout_row(&spec, 120, &chars)],
    }
    .fingerprint();
    assert_eq!(other_cols & 1, 1);
    let busy_at = |busy: bool| {
        let mut s = spec.clone();
        s.meter = Some((None, ""));
        s.busy = busy;
        Presentation {
            cols: 160,
            rows: vec![layout_row(&s, 160, &chars)],
        }
        .fingerprint()
    };
    for busy in [false, true] {
        let fp = busy_at(busy);
        assert_eq!(fp & 1, 1);
        assert_eq!(fp, busy_at(busy));
    }
}

#[test]
fn a_busy_row_is_a_whole_row_track_that_costs_the_words_nothing() {
    let flow = RowSpec {
        kind: RowKind::Message(id(9)),
        severity: Severity::Info,
        live: true,
        glyph: '\u{21bb}',
        title: "Installing aterm v0.92.0",
        detail0: Some("installs within a minute \u{2014} keep working"),
        step: false,
        meter: Some((None, "")),
        busy: true,
        animated: false,
        eta: false,
        load: None,
        load_slot: Loads::NONE,
        capsules: caps(&[]),
    };
    let with_stats = RowSpec {
        detail0: Some("downloading"),
        step: false,
        meter: Some((None, "45 MB")),
        ..flow.clone()
    };
    let bare = RowSpec {
        detail0: None,
        step: false,
        ..with_stats.clone()
    };
    for spec in [&flow, &with_stats, &bare] {
        let still = RowSpec {
            busy: false,
            ..spec.clone()
        };
        for cols in (1..=200).rev() {
            let row = layout_row(spec, cols, &chars);
            assert_eq!(
                chars(&render(&row, cols)),
                cols,
                "{cols}: exactly cols wide"
            );
            assert_eq!(row.glyph.0, GLYPH_COL, "{cols}");
            assert!(row.busy, "{cols}: the comet runs wherever the row is");
            assert!(row.meter.is_none() && row.pct.is_none(), "{cols}: no fill");
            assert_eq!(row.track, Some((0, cols)), "{cols}: the track is the row");
            let plain = layout_row(&still, cols, &chars);
            assert!(!plain.busy && plain.track.is_none(), "{cols}: not busy");
            let slot = 1 + row.elapsed_width();
            if row.elapsed.is_some() && cols > slot {
                let narrower = layout_row(&still, cols - slot, &chars);
                let excerpt = |r: &RowLayout| r.detail.as_ref().map_or(0, |d| chars(&d.1));
                assert!(
                    excerpt(&row) >= excerpt(&narrower),
                    "{cols}: the track took from the excerpt: {:?} vs {:?}",
                    row.detail,
                    narrower.detail
                );
                if spec.detail0.is_none() {
                    let words =
                        |r: &RowLayout| (r.title.clone(), r.stats.as_ref().map(|s| s.1.clone()));
                    assert_eq!(
                        words(&row),
                        words(&narrower),
                        "{cols}: the track bought nothing from the words"
                    );
                }
            }
        }
        assert_eq!(
            layout_row(spec, 0, &chars).track,
            None,
            "no columns, no track"
        );
    }
    let filled = RowSpec {
        meter: Some((Some(500), "")),
        ..flow
    };
    let row = layout_row(&filled, 160, &chars);
    assert!(!row.busy && row.track.is_none());
    assert_eq!(row.meter, Some((0, 160, 500)));
}

fn wide(s: &str) -> usize {
    s.chars()
        .map(|c| match c {
            '\u{300}'..='\u{36f}' => 0,
            '\u{6f22}' => 2,
            _ => 1,
        })
        .sum()
}

#[test]
fn a_wide_excerpt_does_not_flicker_while_narrowing() {
    let detail = format!("{}{}", "ok ".repeat(12), "\u{6f22}".repeat(30));
    let spec = RowSpec {
        kind: RowKind::Message(id(1)),
        severity: Severity::Info,
        live: false,
        glyph: '\u{2139}',
        title: "Installing",
        detail0: Some(&detail),
        step: false,
        meter: None,
        animated: false,
        busy: false,
        eta: false,
        load: None,
        load_slot: Loads::NONE,
        capsules: vec![CapsuleSpec::details(0)],
    };
    let mut pattern = String::new();
    for cols in (30..=140).rev() {
        let row = layout_row(&spec, cols, &wide);
        pattern.push(if row.detail.is_some() { '#' } else { '.' });
    }
    let flips = pattern.matches(".#").count();
    assert_eq!(
        flips, 0,
        "the excerpt returned {flips} time(s) while narrowing 140→30 (# shown, . dropped):\n{pattern}"
    );
    assert!(
        pattern.starts_with("###") && pattern.ends_with("..."),
        "{pattern}"
    );
    let row = layout_row(&spec, 100, &wide);
    let (col, shown) = row.detail.as_ref().unwrap();
    let first = row.capsules.first().unwrap().col;
    assert!(col + wide(shown) + BEFORE_CAPSULES <= first, "{shown:?}");
    assert!(
        shown.ends_with('\u{2026}') && shown.starts_with("ok ok"),
        "{shown:?}"
    );
}

#[test]
fn the_meter_is_the_whole_row_at_every_width_and_costs_the_words_nothing() {
    for animated in [false, true] {
        let spec = |fill: Option<u16>| RowSpec {
            kind: RowKind::Message(id(1)),
            severity: Severity::Info,
            live: animated,
            glyph: '\u{21e3}',
            title: "Installing ALab tools",
            detail0: Some("trust \u{00b7} extracting 120 MB of 900 MB"),
            step: false,
            meter: Some((fill, "")),
            busy: false,
            animated,
            eta: false,
            load: None,
            load_slot: Loads::NONE,
            capsules: vec![
                CapsuleSpec::authored(&Intent::OpenConfigEditor { line: None }, 0),
                CapsuleSpec::details(0),
            ],
        };
        for cols in (1..=120).rev() {
            for fill in [0u16, 1, 420, 999, 1000, 1500] {
                let row = layout_row(&spec(Some(fill)), cols, &chars);
                assert_eq!(
                    row.meter,
                    Some((0, cols, fill.min(1000))),
                    "@{cols} fill {fill}: {}",
                    render(&row, cols)
                );
            }
            let metered = layout_row(&spec(Some(420)), cols, &chars);
            if metered.pct.is_some() && cols > PCT_W {
                let unmetered = layout_row(&spec(None), cols - PCT_W, &chars);
                assert_eq!(unmetered.meter, None, "no fill, no meter");
                if animated {
                    let excerpt = |r: &RowLayout| r.detail.as_ref().map_or(0, |d| chars(&d.1));
                    assert!(
                        excerpt(&metered) >= excerpt(&unmetered),
                        "@{cols}: the meter took from the excerpt"
                    );
                } else {
                    assert_eq!(
                        metered.detail, unmetered.detail,
                        "@{cols}: the meter bought nothing from the excerpt"
                    );
                }
            }
        }
        assert_eq!(layout_row(&spec(Some(420)), 0, &chars).meter, None);
    }
}

#[test]
fn the_eta_slot_goes_short_before_it_goes() {
    let fixtures = motion_fixtures();
    let spec = &fixtures.iter().find(|(n, _)| *n == "first-run").unwrap().1;
    let at = |cols: usize| layout_row(spec, cols, &chars);
    let wide = at(120);
    assert!(wide.eta.is_some() && !wide.eta_short, "long at 120");
    assert_eq!(wide.eta_width(), ETA_W);
    let short: Vec<usize> = (20..=120).filter(|c| at(*c).eta_short).collect();
    assert!(
        !short.is_empty(),
        "the short form is laid out at some width"
    );
    for &cols in &short {
        let narrow = at(cols);
        assert_eq!(narrow.eta_width(), ETA_SHORT_W, "@{cols}");
        assert!(narrow.detail.is_none() && narrow.stats.is_none(), "@{cols}");
    }
    assert!(
        short.iter().any(|c| at(*c).load.is_some()),
        "short beside the load words"
    );
    assert!(
        short.iter().any(|c| at(*c).load_slot.is_none()),
        "short in the load slot's cells"
    );
    for cols in 20..=120 {
        let row = at(cols);
        assert_eq!(
            row.eta.is_some(),
            row.pct.is_some(),
            "@{cols}: the ETA goes only with the activity's words"
        );
    }
}

#[test]
fn an_echo_row_is_hit_nothing() {
    let mut spec = fixtures()
        .into_iter()
        .find(|(n, _)| *n == "toolchain")
        .unwrap()
        .1;
    spec.kind = RowKind::Echo(id(6));
    let p = Presentation {
        cols: 80,
        rows: vec![layout_row(&spec, 80, &chars)],
    };
    for col in 0..90 {
        assert_eq!(p.hit(0, col), Hit::Nothing, "col {col}");
    }
}

#[test]
fn the_load_slot_is_sized_to_the_rows_declared_loads() {
    let build = |slot: Loads| RowSpec {
        kind: RowKind::Message(id(20)),
        severity: Severity::Info,
        live: true,
        glyph: '\u{2139}',
        title: "Building aterm and tools",
        detail0: None,
        step: false,
        meter: Some((None, "3 of 10 targets")),
        animated: false,
        busy: true,
        eta: false,
        load: Some(Load::Cpu),
        load_slot: slot,
        capsules: caps(&[]),
    };
    let cpu = Loads::NONE.with(Load::Cpu);
    let slot_w = |slot: Loads, cols: usize| {
        layout_row(&build(slot), cols, &chars)
            .load_slot
            .map(|(_, w)| w)
    };
    assert_eq!(slot_w(cpu, 80), Some(chars("CPU busy")));
    assert_eq!(slot_w(Loads::ALL, 80), Some(chars("network busy")));
    assert_eq!(
        slot_w(cpu.with(Load::Network), 80),
        Some(chars("network busy")),
        "a second kind widens it to the wider words"
    );
    let mut lull = build(cpu);
    lull.load = None;
    let (with, without) = (
        layout_row(&build(cpu), 80, &chars),
        layout_row(&lull, 80, &chars),
    );
    assert_eq!(with.load_slot, without.load_slot);
    assert_eq!(with.stats, without.stats);
    assert!(without.load.is_none());
    let pass = |cols: usize| {
        layout_row(
            &RowSpec {
                kind: RowKind::Message(id(21)),
                severity: Severity::Info,
                live: true,
                glyph: '\u{21e3}',
                title: "Installing ALab tools",
                detail0: None,
                step: false,
                meter: Some((Some(420), "3 of 10 programs")),
                animated: true,
                busy: false,
                eta: true,
                load: Some(Load::Disk),
                load_slot: [Load::Network, Load::Disk, Load::Cpu].into_iter().collect(),
                capsules: caps(&[]),
            },
            cols,
            &chars,
        )
    };
    assert_eq!(
        pass(80).load_slot.map(|(_, w)| w),
        Some(chars("network busy"))
    );
    assert!(pass(80).eta.is_some(), "80: the ETA first");
    assert_eq!(pass(80).stats, None);
    assert!(
        pass(120).eta.is_some()
            && pass(120).stats.map(|s| s.1).as_deref() == Some("3 of 10 programs"),
        "120: both, long"
    );
    assert!(pass(60).stats.is_none());
    assert_eq!(
        short_stats("3 of 10 programs").as_deref(),
        Some("3/10 programs")
    );
    assert_eq!(
        short_stats("5.9 of 8 cores").as_deref(),
        Some("5.9/8 cores")
    );
    assert_eq!(short_stats("23 of 24 GB used").as_deref(), Some("23/24 GB"));
    for none in [
        "31 MB / 74 MB",
        "  5 MB / 1.2 GB",
        "one of two",
        "3 of",
        "",
        "of 3 of 4",
        "10 programs \u{b7} ",
        "1,200 of 3,400",
        "3 of 10",
    ] {
        assert_eq!(short_stats(none), None, "{none:?}");
    }
    assert_eq!(
        short_stats("10 programs \u{b7} ~3 GB").as_deref(),
        Some("~3 GB")
    );
    assert_eq!(
        short_stats("16 GB \u{b7} 3 of 10 programs").as_deref(),
        Some("3/10 programs")
    );
    assert_eq!(
        short_stats("throttled \u{b7} 6 of 8 cores").as_deref(),
        Some("6/8 cores")
    );
    assert_eq!(
        short_stats("3 of 10 \u{b7} 512 MB / 1.2 GB").as_deref(),
        Some("512 MB / 1.2 GB")
    );
}

#[test]
fn the_eta_outranks_the_load_words_as_the_row_narrows() {
    let spec = |load: Option<Load>| RowSpec {
        kind: RowKind::Message(id(22)),
        severity: Severity::Info,
        live: true,
        glyph: '\u{2193}',
        title: "Updating ALab tools",
        detail0: None,
        step: false,
        meter: Some((Some(660), "1 of 2 programs")),
        animated: true,
        busy: false,
        eta: true,
        load,
        load_slot: [Load::Network, Load::Disk, Load::Cpu].into_iter().collect(),
        capsules: caps(&[]),
    };
    let at60 = layout_row(&spec(Some(Load::Network)), 60, &chars);
    assert!(at60.eta.is_some() && at60.eta_short, "60: the short ETA");
    assert!(
        at60.load.is_none() && at60.load_slot.is_none(),
        "60: no load words"
    );
    let at80 = layout_row(&spec(Some(Load::Network)), 80, &chars);
    assert!(at80.eta.is_some() && !at80.eta_short, "80: the long ETA");
    assert!(at80.load.is_some(), "80: and the load words");
    let (mut eta_gone, mut load_gone, mut short) = (false, false, false);
    for cols in (1..=160).rev() {
        let with = layout_row(&spec(Some(Load::Network)), cols, &chars);
        let without = layout_row(&spec(None), cols, &chars);
        assert_eq!(
            with.load_slot, without.load_slot,
            "{cols}: words never re-lay"
        );
        assert_eq!(with.eta, without.eta, "{cols}");
        assert_eq!(with.stats, without.stats, "{cols}");
        if with.pct.is_some() {
            assert!(
                with.eta.is_some() || with.load_slot.is_none(),
                "{cols}: {with:?}"
            );
        }
        assert!(
            !(eta_gone && with.eta.is_some()),
            "{cols}: the ETA came back"
        );
        assert!(
            !(load_gone && with.load_slot.is_some()),
            "{cols}: the load came back"
        );
        assert!(
            !(short && with.eta.is_some() && !with.eta_short),
            "{cols}: long again"
        );
        eta_gone |= with.eta.is_none();
        load_gone |= with.load_slot.is_none();
        short |= with.eta_short;
        assert!(
            render(&with, cols).chars().count() == cols,
            "{cols}: exactly the row"
        );
    }
    let install = |cols: usize| {
        layout_row(
            &RowSpec {
                kind: RowKind::Message(id(23)),
                severity: Severity::Info,
                live: true,
                glyph: '\u{21e3}',
                title: "Installing ALab tools",
                detail0: None,
                step: false,
                meter: Some((None, "10 programs \u{b7} ~3 GB")),
                animated: false,
                busy: true,
                eta: false,
                load: Some(Load::Network),
                load_slot: [Load::Network, Load::Disk, Load::Cpu].into_iter().collect(),
                capsules: caps(&[]),
            },
            cols,
            &chars,
        )
    };
    assert_eq!(
        install(120).stats.map(|s| s.1).as_deref(),
        Some("10 programs \u{b7} ~3 GB")
    );
    assert_eq!(install(80).stats.map(|s| s.1).as_deref(), Some("~3 GB"));
    assert!(install(80).load.is_some(), "80: the load words too");
}

fn latin_cuts() -> String {
    let corpus = [
        "Couldn't use your keybindings file",
        "Keybinding conflict: \u{2318}K",
        "Claude usage limit reached, resets at 4pm",
        "3 keybindings ignored",
        "aterm closed unexpectedly \u{b7} last session restored",
        "Update ready \u{2014} restart to finish",
        "A-very-long-hyphenated-title here",
        "word",
        "",
        "two  spaces, here.",
    ];
    let mut out = String::new();
    for text in corpus {
        let n = text.chars().count();
        for max in 0..=n + 1 {
            out.push_str(&format!(
                "T {text:?} {max} => {:?}\n",
                title_in(text, max, &crate::text::char_width)
            ));
            out.push_str(&format!(
                "M {text:?} {max} => {:?}\n",
                crate::text::trim_words(text, max)
            ));
            out.push_str(&format!(
                "S {text:?} {max} => {:?}\n",
                crate::text::split_sentence(text, max)
            ));
        }
        out.push_str(&format!("F {text:?} => {:?}\n", first_word(text)));
    }
    out
}

#[test]
fn latin_text_is_cut_where_it_always_was() {
    let golden = include_str!("../tests/fixtures/latin-cuts.txt");
    let now = latin_cuts();
    let moved: Vec<(&str, &str)> = golden
        .lines()
        .zip(now.lines())
        .filter(|(was, is)| was != is)
        .take(8)
        .collect();
    assert!(moved.is_empty(), "moved cuts (was, is): {moved:#?}");
    assert_eq!(golden.lines().count(), now.lines().count());
}

#[test]
fn an_all_cjk_title_is_cut_between_its_characters() {
    let cells = |s: &str| aterm_grapheme::str_width(s);
    let title = "設定ファイルを読み込めませんでした";
    let whole = cells(title);
    for max in 3..whole {
        let cut = title_in(title, max, &cells).expect("some characters fit");
        let stem = cut.strip_suffix('\u{2026}').expect("cut with an ellipsis");
        assert!(title.starts_with(stem) && !stem.is_empty(), "{max}: {cut}");
        assert!(cells(&cut) <= max, "{max}: {cut} is {} cells", cells(&cut));
        assert!(whole_words_of(&cut, title), "{max}: {cut}");
        let next = aterm_grapheme::word_breaks(title)
            .into_iter()
            .find(|brk| brk.at > stem.len())
            .map_or(title.len(), |brk| brk.at);
        if next < title.len() {
            assert!(
                cells(&format!("{}\u{2026}", &title[..next])) > max,
                "{max}: {cut}"
            );
        }
    }
    assert_eq!(title_in(title, whole, &cells).as_deref(), Some(title));
    assert_eq!(first_word(title), "設\u{2026}");
    let quoted = "「設定」を保存しました。";
    for max in 3..cells(quoted) {
        if let Some(cut) = title_in(quoted, max, &cells) {
            let stem = cut.trim_end_matches('\u{2026}');
            assert!(!stem.ends_with('「'), "{max}: {cut}");
            let after = quoted[stem.len()..]
                .trim_start_matches(TITLE_CUT_TRAIL)
                .chars()
                .next();
            assert!(
                !matches!(after, Some('」' | '。')),
                "{max}: {cut} leaves {after:?} to start the rest"
            );
        }
    }
    let spaces_only = |title: &str, max: usize| {
        title
            .match_indices(' ')
            .map(|(at, _)| format!("{}\u{2026}", &title[..at]))
            .take_while(|cut| cells(cut) <= max)
            .last()
    };
    assert_eq!(spaces_only(title, whole - 1), None, "negative control");
}
