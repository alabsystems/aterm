// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Differential VT oracle tests against `astream-term` ([`caps::Caps`]).

mod caps;
mod corpus;
mod engines;
mod exclusions;
mod generator;

use caps::Caps;
use corpus::{CORPUS, Case};
use engines::{Astream, Aterm, Snap};
use exclusions::{EXCLUSIONS, Triage};
use std::fmt::Write as _;

/// The first prefix of `case` at which the engines disagree: the byte count
/// fed, what differs, and both screens.
fn first_divergence(case: &Case) -> Option<(usize, String, Snap, Snap)> {
    let mut a = Astream::new(case.cols, case.rows);
    let mut t = Aterm::new(case.cols, case.rows);
    for (i, b) in case.bytes.iter().enumerate() {
        a.feed(std::slice::from_ref(b));
        t.feed(std::slice::from_ref(b));
        let (sa, st) = (a.snap(), t.snap());
        if let Some(what) = sa.first_difference(&st) {
            return Some((i + 1, what, sa, st));
        }
    }
    None
}

/// aterm's screen after all of `case`, fed a byte at a time as the
/// comparison feeds it.
fn aterm_after(case: &Case) -> Snap {
    let mut t = Aterm::new(case.cols, case.rows);
    for b in case.bytes {
        t.feed(std::slice::from_ref(b));
    }
    t.snap()
}

/// The report for a case that diverged with no exclusion to cover it.
fn unexcused(case: &Case, fed: usize, what: &str, sa: &Snap, st: &Snap) -> String {
    let mut msg = format!(
        "{}: {}x{} (cols x rows), diverged after {fed} of {} bytes: {what}\n  bytes: {}\n",
        case.name,
        case.cols,
        case.rows,
        case.bytes.len(),
        generator::literal(&case.bytes[..fed]),
    );
    let _ = write!(
        msg,
        "  astream-term (cursor {:?}):\n{}  aterm (cursor {:?}):\n{}  \
         triage it: fix aterm (with a regression test), fix astream-term on \
         an astream branch and exclude it here meanwhile (Triage::Astream names \
         the branch), or exclude it as a deliberate difference with the xterm \
         reference",
        sa.cursor,
        sa.text_block(),
        st.cursor,
        st.text_block()
    );
    msg
}

#[test]
fn the_corpus_agrees_or_is_excluded() {
    let caps = Caps::probe();
    let mut failures = Vec::new();
    for case in CORPUS {
        let divergence = first_divergence(case);
        let Some(e) = exclusions::find(case.name) else {
            if let Some((fed, what, sa, st)) = divergence {
                failures.push(unexcused(case, fed, &what, &sa, &st));
            }
            continue;
        };
        let end = aterm_after(case);
        if let Some(missed) = e.want.missed_by(&end) {
            failures.push(format!(
                "{}: aterm no longer gives the written answer ({missed}); that is an \
                 aterm regression, so fix aterm and keep the entry\n  bytes: {}\n  \
                 aterm (cursor {:?}):\n{}  reference: {}",
                case.name,
                generator::literal(case.bytes),
                end.cursor,
                end.text_block(),
                e.why
            ));
        }
        match (&e.triage, divergence) {
            (Triage::Astream { branch, fixed }, None) if !fixed(&caps) => failures.push(format!(
                "{}: the engines agree, but the pinned astream-term still lacks the \
                 fix from {branch} (its Caps probe has not moved), so aterm \
                 has moved onto the fold's known-wrong answer: fix aterm and keep the \
                 entry ({})",
                case.name, e.why
            )),
            (Triage::ByDesign { diverges }, None) if diverges(&caps) => failures.push(format!(
                "{}: excluded as a deliberate difference, but the engines agree: \
                 aterm lost the behaviour its entry defends (restore it), or the \
                 difference was retired on purpose (then delete the entry and say \
                 why) ({})",
                case.name, e.why
            )),
            (Triage::ByDesign { diverges }, Some((fed, what, sa, st))) if !diverges(&caps) => {
                failures.push(unexcused(case, fed, &what, &sa, &st));
            }
            _ => {}
        }
    }
    for e in EXCLUSIONS {
        if !CORPUS.iter().any(|c| c.name == e.case) {
            failures.push(format!(
                "exclusion {}: no corpus case has that name",
                e.case
            ));
        }
        if let Triage::Astream { branch, fixed } = e.triage
            && fixed(&caps)
        {
            failures.push(format!(
                "exclusion {}: the pinned astream-term now carries the fix from \
                 {branch}; delete the entry (the case is held to agree)",
                e.case
            ));
        }
    }
    let mut names: Vec<&str> = CORPUS.iter().map(|c| c.name).collect();
    names.sort_unstable();
    for pair in names.windows(2) {
        if pair[0] == pair[1] {
            failures.push(format!("corpus case {} is named twice", pair[0]));
        }
    }
    let mut excluded: Vec<&str> = EXCLUSIONS.iter().map(|e| e.case).collect();
    excluded.sort_unstable();
    for pair in excluded.windows(2) {
        if pair[0] == pair[1] {
            failures.push(format!("exclusion {} is listed twice", pair[0]));
        }
    }
    assert!(
        failures.is_empty(),
        "the differential VT oracle (caps {caps:?}):\n\n{}",
        failures.join("\n\n")
    );
}

/// Programs per run by default: about a second and a half in a debug build.
const DEFAULT_CASES: u64 = 20_000;

fn env_u64(name: &str) -> Option<u64> {
    let v = std::env::var(name).ok()?;
    let v = v.trim();
    let parsed = match v.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => v.parse(),
    };
    Some(parsed.unwrap_or_else(|e| panic!("{name}={v:?}: {e}")))
}

#[test]
fn random_subset_programs_agree() {
    let caps = Caps::probe();
    let base = env_u64("ATERM_VT_ORACLE_SEED").unwrap_or(0);
    let cases = env_u64("ATERM_VT_ORACLE_CASES").unwrap_or(DEFAULT_CASES);
    for i in 0..cases {
        let seed = base.wrapping_add(i);
        let program = generator::program(seed, caps);
        if let Some(first) = generator::run(&program) {
            let (small, why) = generator::shrink(&program, &first, caps);
            panic!(
                "{}\n  caps {caps:?}\n  replay: ATERM_VT_ORACLE_SEED={seed:#x} \
                 ATERM_VT_ORACLE_CASES=1 targo --unverified test -p aterm-conformance \
                 --test vt -- vt_oracle::random_subset_programs_agree --exact",
                generator::report(seed, (&program, &first), &small, &why)
            );
        }
    }
}

/// The written answers are not vacuous either (a negative control for
/// [`exclusions::Want::missed_by`]): each accepts aterm's own screen for its
/// case and rejects that screen with one glyph changed, the cursor moved, one
/// cell's rendition changed, or the other buffer up.
#[test]
fn the_written_answers_see_text_cursor_rendition_and_buffer() {
    for e in EXCLUSIONS {
        let Some(case) = CORPUS.iter().find(|c| c.name == e.case) else {
            continue; // the corpus check reports it
        };
        let good = aterm_after(case);
        if e.want.missed_by(&good).is_some() {
            continue; // the corpus check reports it
        }
        let mut glyph = good.clone();
        glyph.cells[0].ch = if glyph.cells[0].ch == '#' { '%' } else { '#' };
        let mut cursor = good.clone();
        cursor.cursor.1 = if cursor.cursor.1 == 0 {
            1
        } else {
            cursor.cursor.1 - 1
        };
        let mut ink = good.clone();
        ink.cells[0].ink.underline = !ink.cells[0].ink.underline;
        let mut buffer = good.clone();
        buffer.alt_screen = !buffer.alt_screen;
        for (what, bad) in [
            ("glyph", glyph),
            ("cursor", cursor),
            ("rendition", ink),
            ("buffer", buffer),
        ] {
            assert!(
                e.want.missed_by(&bad).is_some(),
                "{}: the written answer does not see a changed {what}",
                e.case
            );
        }
    }
}

/// The shrinker's guard is not vacuous: [`generator::Program::in_subset`]
/// rejects the programs a dropped token makes out of good ones (a second
/// `?1049h` once the `?1049l` between is gone, a `?1049l` on the main screen
/// once its `?1049h` is gone, an alternate-screen DECRC once its DECSC is
/// gone), with the capabilities of a fold that has none of the widenings.
#[test]
fn the_subset_check_sees_what_a_dropped_token_rebuilds() {
    use generator::{Kind, Program, Token};
    let narrow = Caps {
        erase_resets_wrap: false,
        ed3_spares_screen: false,
        bce_on_scroll: false,
        vt_ff_feed: false,
        index_ops: false,
        insert_delete: false,
        decsc_saves_pen: false,
        alt_persists: false,
        ris_keeps_alt: false,
        alt_clears: false,
        utf8_subparts: false,
        origin_mode: false,
        insert_mode: false,
        scroll_up_down: false,
        newline_mode: false,
        charsets: false,
    };
    let program = |kinds: &[Kind]| Program {
        cols: 4,
        rows: 2,
        tokens: kinds
            .iter()
            .map(|&kind| Token {
                kind,
                chunks: vec![],
            })
            .collect(),
    };
    let enter = Kind::Alt {
        mode: 1049,
        set: true,
    };
    let leave = Kind::Alt {
        mode: 1049,
        set: false,
    };
    for (good, dropped) in [
        (vec![enter, leave, enter], 1),
        (vec![enter, leave], 0),
        (vec![enter, Kind::Save, Kind::Restore], 1),
        (
            vec![enter, leave, Kind::Reset, enter, Kind::Save, Kind::Restore],
            4,
        ),
    ] {
        assert!(
            program(&good).in_subset(narrow),
            "{good:?} is in the subset"
        );
        let mut bad = good.clone();
        bad.remove(dropped);
        assert!(!program(&bad).in_subset(narrow), "{bad:?} is not");
    }
    let wide = Caps {
        alt_persists: true,
        alt_clears: true,
        ..narrow
    };
    assert!(program(&[enter, Kind::Restore]).in_subset(wide));
    assert!(program(&[enter, enter]).in_subset(wide));
    assert!(!program(&[leave]).in_subset(wide));
    let ris = Caps {
        ris_keeps_alt: true,
        ..wide
    };
    let enter_47 = Kind::Alt {
        mode: 47,
        set: true,
    };
    assert!(program(&[Kind::Reset, enter_47]).in_subset(wide));
    assert!(!program(&[Kind::Reset, enter_47]).in_subset(ris));
    assert!(program(&[Kind::Reset, enter]).in_subset(ris));
    assert!(program(&[Kind::Reset, enter, Kind::Restore]).in_subset(wide));
    assert!(!program(&[Kind::Reset, enter, Kind::Restore]).in_subset(ris));
    assert!(program(&[Kind::Reset, enter, Kind::Save, leave, enter, Kind::Restore]).in_subset(ris));
}

/// The comparison is not vacuous: it sees a one-glyph difference, a cursor
/// difference, a colour difference and a visibility difference (a negative
/// control for `engines.rs`, whose projections could otherwise drift into
/// reading nothing).
#[test]
fn the_comparison_sees_text_cursor_colour_and_visibility() {
    let snap = |a: &[u8]| {
        let mut t = Aterm::new(6, 2);
        t.feed(a);
        t.snap()
    };
    let base = snap(b"ab");
    assert!(base.first_difference(&snap(b"ab")).is_none());
    assert!(base.first_difference(&snap(b"ac")).is_some(), "text");
    assert!(
        base.first_difference(&snap(b"ab\x1b[D")).is_some(),
        "cursor"
    );
    assert!(
        base.first_difference(&snap(b"\x1b[31mab")).is_some(),
        "colour"
    );
    assert!(
        base.first_difference(&snap(b"ab\x1b[?25l")).is_some(),
        "visibility"
    );
    let mut a = Astream::new(6, 2);
    a.feed(b"ab");
    assert!(
        a.snap().first_difference(&base).is_none(),
        "and the two agree on it"
    );
}
