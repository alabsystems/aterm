// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::ribbon::Layer;
use super::*;

const fn cell(row: u16, col: u16, born: Instant) -> Cell {
    cell_of(row, col, born, 0)
}

const fn cell_of(row: u16, col: u16, born: Instant, cohort: u32) -> Cell {
    Cell {
        row,
        col,
        cohort,
        t: 0.0,
        born,
        attack_at: born,
        life_s: 1.0,
        cov0: 1.0,
        typing: true,
        retract_at: None,
        retire_at: None,
        birth_disp: 0.5,
        edge_cells: 3.0,
        layer: Layer::Base,
        rearm: None,
        released_at: None,
    }
}

fn row(s: &str) -> Vec<char> {
    s.chars().collect()
}

fn pos(out: &[(u16, u16, Instant)]) -> Vec<(u16, u16)> {
    out.iter().map(|&(r, c, _)| (r, c)).collect()
}

#[test]
fn a_malformed_or_short_sample_reads_blank() {
    let cols = row("a\u{4f60}\0 ");
    assert!(unit_at(&cols, 3).is_blank(), "a space is blank");
    assert!(unit_at(&cols, 9).is_blank(), "past the sample is blank");
    assert!(
        unit_at(&row("\0x"), 0).is_blank(),
        "a stray continuation is blank"
    );
}

fn armed_run(above: &str) -> (Witness, Vec<Cell>) {
    let t0 = Instant::now();
    let mut w = Witness::new();
    let cells: Vec<Cell> = (0..6).map(|c| cell_of(5, c, t0, 7)).collect();
    let (mut out, mut rel) = (Vec::new(), Vec::new());
    w.walk(
        &cells,
        &[
            RowSample {
                row: 5,
                cols: &row("abcdef"),
            },
            RowSample {
                row: 4,
                cols: &row(above),
            },
        ],
        &[],
        Instant::now(),
        &mut out,
        &mut rel,
    );
    assert!(out.is_empty() && rel.is_empty());
    assert_eq!(w.len(), 6, "every glyph armed");
    (w, cells)
}

fn follow_onto(w: &mut Witness, cells: &[Cell], now: &str) -> Vec<FollowRun> {
    let mut out = Vec::new();
    w.follow_runs(
        cells,
        &[
            RowSample {
                row: 5,
                cols: &row("      "),
            },
            RowSample {
                row: 4,
                cols: &row(now),
            },
        ],
        &mut out,
    );
    out
}

const fn up(lo: u16, hi: u16) -> FollowRun {
    FollowRun {
        row: 5,
        cohort: 7,
        lo,
        hi,
        dr: -1,
    }
}

#[test]
fn a_short_park_cannot_relax_an_unrelated_run() {
    let sample = [
        RowSample {
            row: 5,
            cols: &row("bcdef       "),
        },
        RowSample {
            row: 4,
            cols: &row("a           "),
        },
    ];
    let (mut ordinary, cells) = armed_run("            ");
    let mut out = Vec::new();
    ordinary.follow_runs(&cells, &sample, &mut out);
    assert!(out.is_empty(), "one arrival cannot move an ordinary run");

    let (mut unrelated, cells) = armed_run("            ");
    out.clear();
    unrelated.follow_runs_with_short_park(&cells, &sample, Some(((5, 12), (5, 11))), &mut out);
    assert!(
        out.is_empty(),
        "an unrelated park at col 12 must not relax the run at cols 0..5: {out:?}"
    );

    let (mut owned, cells) = armed_run("            ");
    out.clear();
    owned.follow_runs_with_short_park(&cells, &sample, Some(((5, 6), (5, 5))), &mut out);
    assert_eq!(out, vec![up(0, 0)], "the exact run keeps the narrow wrap");
}

#[test]
fn a_twin_inside_the_block_is_neutral_and_one_off_the_target_is_a_hole() {
    const B: &str = "      ";
    type Case = (
        Option<&'static str>,
        &'static str,
        &'static str,
        Option<(u16, u16)>,
        Option<u32>,
    );
    let cases: [Case; 25] = [
        (Some(B), B, "abcdef", Some((0, 5)), Some(0)),
        (Some(B), B, "abcd  ", Some((0, 3)), None),
        (Some(B), B, "  cdef", Some((2, 5)), None),
        (Some(B), B, "abc  f", None, Some(6)),
        (Some(B), B, "ab  ef", None, None),
        (Some(B), B, "a cdef", None, None),
        (Some(B), B, "uvwxyz", None, Some(0)),
        (Some(B), "abcdef", "abcdef", None, Some(0)),
        (None, B, "abcdef", Some((0, 5)), None),
        (Some("abcdef"), B, "abcdef", None, Some(0)),
        (Some("abc   "), B, "abcxyz", None, Some(0)),
        (Some("  c   "), B, "abcdef", Some((0, 5)), Some(0)),
        (Some("a    f"), B, "abcdef", Some((0, 5)), None),
        (Some(" bcde "), B, "abcdef", Some((0, 5)), None),
        (Some("  c   "), B, "ab def", None, None),
        (Some("  c   "), B, "ab  ef", None, None),
        (Some("a     "), "a     ", "abcdef", Some((0, 5)), None),
        (Some("     f"), "     f", "abcdef", Some((0, 5)), None),
        (Some("a     "), "a     ", "a cdef", Some((2, 5)), None),
        (Some("a     "), B, "a cdef", Some((2, 5)), None),
        (Some("     f"), "     f", "abcd f", Some((0, 3)), None),
        (Some("     f"), B, "abcd f", Some((0, 3)), None),
        (Some("abcd  "), B, "abcdef", Some((0, 5)), None),
        (Some("abcdxy"), B, "abcdxy", None, None),
        (Some("abcd  "), B, "abcde ", None, None),
    ];
    let own_run = row("abcdef");
    for (above, own, now, named, missed) in cases {
        let ctx = format!("armed under {above:?}, then `{own}` / `{now}`");
        let above = above.map(row);
        let mut arming = vec![RowSample {
            row: 5,
            cols: &own_run,
        }];
        if let Some(cols) = &above {
            arming.push(RowSample { row: 4, cols });
        }
        let (mut w, cells) = armed_on(5, &arming);
        let (own, now) = (row(own), row(now));
        let mut out = Vec::new();
        let score = w.follow_runs(
            &cells,
            &[
                RowSample { row: 5, cols: &own },
                RowSample { row: 4, cols: &now },
            ],
            &mut out,
        );
        let want: Vec<FollowRun> = named.map(|(lo, hi)| up(lo, hi)).into_iter().collect();
        assert_eq!(out, want, "{ctx}");
        if let Some(missed) = missed {
            assert_eq!(score.missed, missed, "{ctx}: missed");
        }
    }
}

fn armed_line_under(text: &str, above: &str) -> (Witness, Vec<Cell>) {
    let t0 = Instant::now();
    let mut w = Witness::new();
    let n = u16::try_from(text.chars().count()).expect("a short line");
    let cells: Vec<Cell> = (0..n).map(|c| cell_of(5, c, t0, 7)).collect();
    let (mut out, mut rel) = (Vec::new(), Vec::new());
    w.walk(
        &cells,
        &[
            RowSample {
                row: 5,
                cols: &row(text),
            },
            RowSample {
                row: 4,
                cols: &row(above),
            },
        ],
        &[],
        t0,
        &mut out,
        &mut rel,
    );
    assert!(out.is_empty() && rel.is_empty());
    (w, cells)
}

fn follow_line(w: &mut Witness, cells: &[Cell], own: &str, now: &str) -> (Vec<FollowRun>, u32) {
    let mut out = Vec::new();
    let score = w.follow_runs(
        cells,
        &[
            RowSample {
                row: 5,
                cols: &row(own),
            },
            RowSample {
                row: 4,
                cols: &row(now),
            },
        ],
        &mut out,
    );
    (out, score.missed)
}

#[test]
fn a_word_the_wrap_relaid_at_its_row_s_start_does_not_count_against_the_line() {
    let line = "  this confirmatio";
    let blank = "";
    let run = FollowRun {
        row: 5,
        cohort: 7,
        lo: 0,
        hi: 5,
        dr: -1,
    };
    let (mut w, cells) = armed_line_under(line, blank);
    assert_eq!(
        follow_line(&mut w, &cells, "  confirmation", "  this"),
        (vec![run], 0),
        "`this` arrived a row up and `confirmatio` leads the caret row: the line follows"
    );
    for (own, now, want, what) in [
        (
            "  xyzwvutsrqpon",
            "  this",
            (vec![], 0),
            "a rewrite relays nothing",
        ),
        (
            "  xyz confirmatio",
            "  this",
            (vec![], 0),
            "a word not leading its row",
        ),
        (
            "  confirmation",
            "  t is",
            (vec![], 14),
            "a hole in the kept line",
        ),
    ] {
        let (mut w, cells) = armed_line_under(line, blank);
        assert_eq!(follow_line(&mut w, &cells, own, now), want, "{what}");
    }
}

fn armed_on(row: u16, rows: &[RowSample<'_>]) -> (Witness, Vec<Cell>) {
    let t0 = Instant::now();
    let mut w = Witness::new();
    let cells: Vec<Cell> = (0..6).map(|c| cell_of(row, c, t0, 7)).collect();
    walk_quiet(&mut w, &cells, rows);
    assert_eq!(w.len(), 6, "every glyph armed");
    (w, cells)
}

fn walk_quiet(w: &mut Witness, cells: &[Cell], rows: &[RowSample<'_>]) {
    walk_quiet_at(w, cells, rows, 0);
}

fn walk_quiet_at(w: &mut Witness, cells: &[Cell], rows: &[RowSample<'_>], ms: u64) {
    let (mut out, mut rel) = (Vec::new(), Vec::new());
    let now = cells[0].born + Duration::from_millis(ms);
    w.walk(cells, rows, &[], now, &mut out, &mut rel);
    assert!(out.is_empty() && rel.is_empty(), "the glyphs stand");
}

fn whole_up() -> Vec<FollowRun> {
    vec![FollowRun {
        row: 5,
        cohort: 7,
        lo: 0,
        hi: 5,
        dr: -1,
    }]
}

#[test]
fn a_copy_that_appears_beside_a_standing_line_is_a_twin_once_it_has_stood_forty_ms() {
    let own = row("abcdef");
    let blank = row("      ");
    let copy = row("abcdef");
    let other = row("xyzxyz");
    let frame = |w: &mut Witness, cells: &[Cell], four: &[char], ms: u64| {
        walk_quiet_at(
            w,
            cells,
            &[
                RowSample { row: 5, cols: &own },
                RowSample { row: 4, cols: four },
            ],
            ms,
        );
    };
    let armed = || {
        armed_on(
            5,
            &[
                RowSample { row: 5, cols: &own },
                RowSample {
                    row: 4,
                    cols: &blank,
                },
            ],
        )
    };
    let (mut w, cells) = armed();
    for ms in [16, 32, 48] {
        frame(&mut w, &cells, &copy, ms);
    }
    assert_eq!(
        follow_onto(&mut w, &cells, "abcdef"),
        whole_up(),
        "a copy that stood 32 ms: the new row of a torn repaint"
    );
    let (mut w, cells) = armed();
    for ms in [16, 32, 48, 64] {
        frame(&mut w, &cells, &copy, ms);
    }
    assert_eq!(
        follow_onto(&mut w, &cells, "abcdef"),
        vec![],
        "the copy stood beside the line: its erase is not a move"
    );
    let (mut w, cells) = armed();
    frame(&mut w, &cells, &copy, 120);
    frame(&mut w, &cells, &copy, 240);
    assert_eq!(follow_onto(&mut w, &cells, "abcdef"), vec![]);
    let (mut w, cells) = armed();
    frame(&mut w, &cells, &copy, 16);
    frame(&mut w, &cells, &other, 48);
    frame(&mut w, &cells, &copy, 80);
    assert_eq!(follow_onto(&mut w, &cells, "abcdef"), whole_up());
    let (mut w, cells) = armed();
    frame(&mut w, &cells, &copy, 16);
    walk_quiet_at(&mut w, &cells, &[RowSample { row: 5, cols: &own }], 32);
    frame(&mut w, &cells, &copy, 64);
    assert_eq!(follow_onto(&mut w, &cells, "abcdef"), vec![]);
    let (mut w, cells) = armed();
    frame(&mut w, &cells, &copy, 16);
    frame(&mut w, &cells, &copy, 64);
    frame(&mut w, &cells, &blank, 80);
    assert_eq!(follow_onto(&mut w, &cells, "abcdef"), vec![]);
}

#[test]
fn a_carried_record_starts_over_on_its_new_row_as_if_just_armed() {
    let text = row("abcdef");
    let blank = row("      ");
    let dests = |out: &[FollowRun]| out.iter().map(|f| (f.row, f.dr)).collect::<Vec<_>>();
    let carried_down = || {
        let (mut w, cells) = armed_run("abcdef");
        let mut out = Vec::new();
        w.follow_runs(
            &cells,
            &[
                RowSample {
                    row: 5,
                    cols: &blank,
                },
                RowSample {
                    row: 4,
                    cols: &text,
                },
                RowSample {
                    row: 6,
                    cols: &text,
                },
            ],
            &mut out,
        );
        assert_eq!(
            dests(&out),
            vec![(5, 1)],
            "fixture: the run follows its text down, not onto its twin"
        );
        let mut moved: Vec<(u16, u16, Instant)> =
            cells.iter().map(|c| (c.row, c.col, c.born)).collect();
        w.translate_cells(&mut moved, 1);
        let carried: Vec<Cell> = cells
            .iter()
            .map(|c| cell_of(6, c.col, c.born, c.cohort))
            .collect();
        (w, carried)
    };
    let (mut w, carried) = carried_down();
    let mut out = Vec::new();
    w.follow_runs(
        &carried,
        &[
            RowSample {
                row: 6,
                cols: &blank,
            },
            RowSample {
                row: 5,
                cols: &text,
            },
            RowSample {
                row: 4,
                cols: &text,
            },
        ],
        &mut out,
    );
    assert_eq!(dests(&out), vec![(6, -1)], "back up one row");
    let (mut w, carried) = carried_down();
    for ms in [16, 64] {
        walk_quiet_at(
            &mut w,
            &carried,
            &[
                RowSample {
                    row: 6,
                    cols: &text,
                },
                RowSample {
                    row: 7,
                    cols: &text,
                },
            ],
            ms,
        );
    }
    let mut out = Vec::new();
    w.follow_runs(
        &carried,
        &[
            RowSample {
                row: 6,
                cols: &blank,
            },
            RowSample {
                row: 7,
                cols: &text,
            },
        ],
        &mut out,
    );
    assert_eq!(dests(&out), vec![], "erased beside a twin it learned");
}

#[test]
fn a_band_move_that_opens_a_row_beside_a_line_leaves_it_unseen_until_a_frame_shows_it() {
    let own = row("abcdef");
    let blank = row("      ");
    let t0 = Instant::now();
    let cells: Vec<Cell> = (0..6).map(|c| cell_of(10, c, t0, 7)).collect();
    let armed = || {
        let mut w = Witness::new();
        walk_quiet(
            &mut w,
            &cells,
            &[
                RowSample {
                    row: 10,
                    cols: &own,
                },
                RowSample {
                    row: 11,
                    cols: &blank,
                },
            ],
        );
        w.translate_band(11, 20, 1);
        w
    };
    let erased_onto_eleven = |w: &mut Witness| {
        let mut out = Vec::new();
        w.follow_runs(
            &cells,
            &[
                RowSample {
                    row: 10,
                    cols: &blank,
                },
                RowSample {
                    row: 11,
                    cols: &own,
                },
            ],
            &mut out,
        );
        out.iter().map(|f| f.dr).collect::<Vec<_>>()
    };
    let mut w = armed();
    assert_eq!(
        erased_onto_eleven(&mut w),
        Vec::<i16>::new(),
        "`S` after `yyp`"
    );
    let mut w = armed();
    walk_quiet_at(
        &mut w,
        &cells,
        &[
            RowSample {
                row: 10,
                cols: &own,
            },
            RowSample {
                row: 11,
                cols: &row("}     "),
            },
        ],
        16,
    );
    assert_eq!(
        erased_onto_eleven(&mut w),
        vec![1],
        "seen without it: a move"
    );
}

#[test]
fn a_row_a_scroll_brings_in_dates_its_copy_from_the_last_frame_before_the_scroll() {
    let own = row("abcdef");
    let blank = row("      ");
    let t0 = Instant::now();
    let at = |r: u16| -> Vec<Cell> { (0..6).map(|c| cell_of(r, c, t0, 7)).collect() };
    let scrolled = || {
        let mut w = Witness::new();
        walk_quiet(
            &mut w,
            &at(23),
            &[RowSample {
                row: 23,
                cols: &own,
            }],
        );
        w.translate(1, 24);
        w
    };
    let erased = |w: &mut Witness| {
        let mut out = Vec::new();
        w.follow_runs(
            &at(22),
            &[
                RowSample {
                    row: 22,
                    cols: &blank,
                },
                RowSample {
                    row: 23,
                    cols: &own,
                },
            ],
            &mut out,
        );
        out.iter().map(|f| f.dr).collect::<Vec<_>>()
    };
    let look = |w: &mut Witness, twenty_three: &[char], ms: u64| {
        walk_quiet_at(
            w,
            &at(22),
            &[
                RowSample {
                    row: 22,
                    cols: &own,
                },
                RowSample {
                    row: 23,
                    cols: twenty_three,
                },
            ],
            ms,
        );
    };
    let mut w = scrolled();
    look(&mut w, &own, 48);
    assert_eq!(
        erased(&mut w),
        Vec::<i16>::new(),
        "the copy came in with the row, 48 ms ago at the latest"
    );
    let mut w = scrolled();
    look(&mut w, &own, 16);
    assert_eq!(erased(&mut w), vec![1], "16 ms: the line relocated, torn");
    let mut w = scrolled();
    look(&mut w, &own, 16);
    look(&mut w, &own, 48);
    assert_eq!(erased(&mut w), Vec::<i16>::new(), "seen again at 48 ms");
    let mut w = scrolled();
    look(&mut w, &blank, 48);
    assert_eq!(erased(&mut w), vec![1], "seen blank first: a move");
    let mut w = scrolled();
    assert_eq!(erased(&mut w), vec![1], "never looked at: a move");
}

#[test]
fn a_run_beside_a_withheld_row_waits_one_walk() {
    let own = row("abcdef");
    let other = row("uvwxyz");
    let blank = row("      ");
    let t0 = Instant::now();
    let cells: Vec<Cell> = (0..6).map(|c| cell_of(5, c, t0, 7)).collect();
    let armed = || {
        let mut w = Witness::new();
        walk_quiet(
            &mut w,
            &cells,
            &[
                RowSample { row: 5, cols: &own },
                RowSample {
                    row: 4,
                    cols: &blank,
                },
            ],
        );
        w
    };
    let replaced = |w: &mut Witness, withheld: &[u16]| {
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &other,
            }],
            withheld,
            t0 + Duration::from_millis(16),
            &mut out,
            &mut rel,
        );
        out.len()
    };
    let mut w = armed();
    assert_eq!(replaced(&mut w, &[4]), 0, "row 4 withheld: no verdict");
    assert_eq!(w.len(), 6, "the records are kept");
    let mut out = Vec::new();
    w.follow_runs(
        &cells,
        &[
            RowSample {
                row: 5,
                cols: &other,
            },
            RowSample { row: 4, cols: &own },
        ],
        &mut out,
    );
    assert_eq!(out.iter().map(|f| f.dr).collect::<Vec<_>>(), vec![-1]);
    let mut w = armed();
    assert_eq!(replaced(&mut w, &[4]), 0);
    assert_eq!(replaced(&mut w, &[4]), 6, "withheld again: judged");
    let mut w = armed();
    assert_eq!(replaced(&mut w, &[]), 6, "nothing withheld: judged at once");
    let mut w = armed();
    assert_eq!(replaced(&mut w, &[9]), 6, "a row not beside it: judged");
}

#[test]
fn a_twin_testifies_only_about_its_own_row() {
    let own = row("abcdef");
    let blank = row("      ");
    let near = row("abcdvw");
    for (four, three, six, lands, named) in [
        (&own, &blank, &blank, 3u16, vec![-2]),
        (&blank, &blank, &blank, 3, vec![-2]),
        (&own, &blank, &blank, 7, vec![2]),
        (&own, &blank, &near, 7, vec![2]),
        (&own, &own, &blank, 3, vec![]),
    ] {
        let (mut w, cells) = armed_on(
            5,
            &[
                RowSample { row: 5, cols: &own },
                RowSample { row: 4, cols: four },
                RowSample {
                    row: 3,
                    cols: three,
                },
                RowSample { row: 6, cols: six },
                RowSample {
                    row: 7,
                    cols: &blank,
                },
            ],
        );
        let at = |r: u16, was: &[char]| -> Vec<char> {
            if r == lands {
                own.clone()
            } else {
                was.to_vec()
            }
        };
        let (r3, r7) = (at(3, three), at(7, &blank));
        let mut out = Vec::new();
        w.follow_runs(
            &cells,
            &[
                RowSample {
                    row: 5,
                    cols: &blank,
                },
                RowSample { row: 4, cols: four },
                RowSample { row: 3, cols: &r3 },
                RowSample { row: 6, cols: six },
                RowSample { row: 7, cols: &r7 },
            ],
            &mut out,
        );
        assert_eq!(
            out.iter().map(|f| f.dr).collect::<Vec<_>>(),
            named,
            "row 4 {:?}, row 3 {:?}, row 6 {:?}, lands on {lands}",
            four.iter().collect::<String>(),
            three.iter().collect::<String>(),
            six.iter().collect::<String>()
        );
    }
}

#[test]
fn a_record_the_shape_pass_caught_up_is_armed_again() {
    let own = row("abcdef");
    let blank = row("      ");
    let t0 = Instant::now();
    let cells: Vec<Cell> = (0..6).map(|c| cell_of(5, c, t0, 7)).collect();
    let caught_up = || {
        let (mut w, _) = armed_on(
            5,
            &[
                RowSample { row: 5, cols: &own },
                RowSample { row: 4, cols: &own },
                RowSample {
                    row: 6,
                    cols: &blank,
                },
            ],
        );
        for (text, ms) in [("abXYef", 100), ("abcdef", 190)] {
            let (mut out, mut rel) = (Vec::new(), Vec::new());
            w.walk(
                &cells,
                &[
                    RowSample {
                        row: 5,
                        cols: &row(text),
                    },
                    RowSample { row: 4, cols: &own },
                    RowSample {
                        row: 6,
                        cols: &blank,
                    },
                ],
                &[],
                t0 + Duration::from_millis(ms),
                &mut out,
                &mut rel,
            );
            assert!(
                out.is_empty() && rel.is_empty(),
                "an interior rewrite is not evidence: {text}"
            );
        }
        w
    };
    let follow = |w: &mut Witness, six: &[char]| {
        let mut out = Vec::new();
        w.follow_runs(
            &cells,
            &[
                RowSample {
                    row: 5,
                    cols: &blank,
                },
                RowSample { row: 4, cols: &own },
                RowSample { row: 6, cols: six },
            ],
            &mut out,
        );
        out.iter().map(|f| f.dr).collect::<Vec<_>>()
    };
    let mut w = caught_up();
    assert_eq!(
        follow(&mut w, &blank),
        Vec::<i16>::new(),
        "erased under its twin"
    );
    let mut w = caught_up();
    assert_eq!(follow(&mut w, &own), vec![1], "moved down a row");
}

fn armed_line(text: &str) -> (Witness, Vec<Cell>) {
    let t0 = Instant::now();
    let mut w = Witness::new();
    let n = u16::try_from(text.chars().count()).expect("a short line");
    let cells: Vec<Cell> = (0..n).map(|c| cell_of(5, c, t0, 7)).collect();
    let (mut out, mut rel) = (Vec::new(), Vec::new());
    w.walk(
        &cells,
        &[RowSample {
            row: 5,
            cols: &row(text),
        }],
        &[],
        t0,
        &mut out,
        &mut rel,
    );
    assert!(out.is_empty() && rel.is_empty());
    (w, cells)
}

fn shift_onto(w: &mut Witness, cells: &[Cell], now: &str, caret: Option<u16>) -> Vec<ShiftRun> {
    let mut out = Vec::new();
    w.shift_runs(
        cells,
        &[RowSample {
            row: 5,
            cols: &row(now),
        }],
        caret.map(|c| (5, c)),
        Instant::now(),
        &mut out,
    );
    out
}

fn shift(lo: u16, hi: u16, dc: i16) -> Vec<ShiftRun> {
    vec![ShiftRun {
        row: 5,
        cohort: 7,
        lo,
        hi,
        dc,
    }]
}

#[test]
fn an_insert_names_the_tail_it_pushed_with_its_width() {
    type Case = (
        &'static str,
        &'static str,
        Option<u16>,
        Option<(u16, u16, i16)>,
    );
    let cases: [Case; 15] = [
        ("hello world", "hello Xworld", Some(6), Some((6, 10, 1))),
        ("hello world", "helloX world", Some(5), Some((5, 10, 1))),
        ("hello world", "hello XYZworld", Some(6), Some((6, 10, 3))),
        ("hello world", "hello world", Some(11), None),
        ("hello", "helllo", Some(2), Some((2, 4, 1))),
        ("hello", "helllo", Some(3), Some((3, 4, 1))),
        ("hello", "helllo", None, None),
        ("abcd", "abbcd", Some(2), Some((2, 3, 1))),
        ("type", "typXe", Some(3), Some((3, 3, 1))),
        ("type", "typXe", None, None),
        ("type", "typXe", Some(1), None),
        ("I als", "I alee see", Some(4), None),
        ("hello world", "hello xyzqw", Some(6), None),
        ("hello world", "hello Xwoqqq", Some(6), None),
        ("ab cd ab", "ab cd   ", Some(8), None),
    ];
    for (armed, now, caret, want) in cases {
        let (mut w, cells) = armed_line(armed);
        let want = want.map_or_else(Vec::new, |(lo, hi, dc)| shift(lo, hi, dc));
        assert_eq!(
            shift_onto(&mut w, &cells, now, caret),
            want,
            "`{armed}` -> `{now}`, caret {caret:?}"
        );
    }
}

#[test]
fn a_shift_onto_its_own_left_behind_tail_leaves_one_record_per_key() {
    let (mut w, cells) = armed_line("hi you there");
    assert_eq!(
        shift_onto(&mut w, &cells, "hi XYZyou      ", Some(3)),
        shift(3, 6, 3)
    );
    let mut moved: Vec<_> = cells
        .iter()
        .filter(|c| (3..=6).contains(&c.col))
        .map(|c| (c.row, c.col, c.born))
        .collect();
    w.shift_cells(&mut moved, 3);
    let keys: Vec<_> = w.seen.iter().map(|s| (s.row, s.col, s.born)).collect();
    let mut dedup = keys.clone();
    dedup.dedup();
    assert_eq!(keys, dedup, "two records share a key");
    let cols: Vec<u16> = w.seen.iter().map(|s| s.col).collect();
    assert_eq!(cols, vec![0, 1, 6, 7, 8, 10, 11]);
    let units: Vec<Unit> = w.seen.iter().map(|s| s.unit).collect();
    let at = |c: u16| units[cols.iter().position(|&x| x == c).expect("a record")];
    assert_eq!(at(6), unit_at(&row("hi you there"), 3), "`y` moved to 6");
    assert_eq!(at(8), unit_at(&row("hi you there"), 5), "`u` moved to 8");
    assert_eq!(at(10), unit_at(&row("hi you there"), 10), "`r` stayed");
}

#[test]
fn shifted_records_find_their_glyphs_on_the_next_walk() {
    let (mut w, mut cells) = armed_line("hello world");
    let found = shift_onto(&mut w, &cells, "hello Xworld", Some(6));
    assert_eq!(found, shift(6, 10, 1));
    let mut moved = Vec::new();
    for c in &mut cells {
        if (6..=10).contains(&c.col) {
            moved.push((c.row, c.col, c.born));
            c.col += 1;
        }
    }
    w.shift_cells(&mut moved, 1);
    let (mut out, mut rel) = (Vec::new(), Vec::new());
    w.walk(
        &cells,
        &[RowSample {
            row: 5,
            cols: &row("hello Xworld"),
        }],
        &[],
        Instant::now(),
        &mut out,
        &mut rel,
    );
    assert!(
        out.is_empty() && rel.is_empty(),
        "retired {:?} released {:?}",
        pos(&out),
        pos(&rel)
    );
}

#[test]
fn blank_to_glyph_only_arms_and_glyph_to_blank_releases() {
    let t0 = Instant::now();
    let mut w = Witness::new();
    let mut out = Vec::new();
    let mut rel = Vec::new();
    let cells = [cell(5, 0, t0), cell(5, 1, t0)];
    w.walk(
        &cells,
        &[RowSample {
            row: 5,
            cols: &row("  "),
        }],
        &[],
        Instant::now(),
        &mut out,
        &mut rel,
    );
    assert!(
        out.is_empty() && w.is_empty(),
        "a blank arms nothing and retires nothing"
    );
    w.walk(
        &cells,
        &[RowSample {
            row: 5,
            cols: &row("hi"),
        }],
        &[],
        Instant::now(),
        &mut out,
        &mut rel,
    );
    assert!(out.is_empty() && w.len() == 2, "seen: armed, not retired");
    w.walk(
        &cells,
        &[RowSample {
            row: 5,
            cols: &row("hi"),
        }],
        &[],
        Instant::now(),
        &mut out,
        &mut rel,
    );
    assert!(
        out.is_empty() && w.len() == 2,
        "the same glyph touches nothing"
    );
    w.walk(
        &cells,
        &[RowSample {
            row: 5,
            cols: &row("  "),
        }],
        &[],
        Instant::now(),
        &mut out,
        &mut rel,
    );
    assert!(out.is_empty(), "a blank is not the fast melt's verdict");
    assert_eq!(pos(&rel), vec![(5, 0), (5, 1)]);
    assert!(
        rel.iter().all(|&(_, _, born)| born == t0),
        "named by identity: the cells' own births"
    );
    assert_eq!(
        w.len(),
        2,
        "released records are KEPT: the cells are resident and unstamped for the retract"
    );
}

type Named = (Vec<(u16, u16)>, Vec<(u16, u16)>);

fn walk_row(w: &mut Witness, cells: &[Cell], r: u16, now: &str) -> Named {
    let (mut out, mut rel) = (Vec::new(), Vec::new());
    w.walk(
        cells,
        &[RowSample {
            row: r,
            cols: &row(now),
        }],
        &[],
        Instant::now(),
        &mut out,
        &mut rel,
    );
    (pos(&out), pos(&rel))
}

#[test]
fn returning_partial_ink_requires_the_released_identity_and_exact_unit() {
    for text in ["abcdef", "ab你\0ef"] {
        let (mut w, cells) = armed_line(text);
        let held = cells[2];
        let original = row(text);
        assert!(!w.released_cell_matches(&held, &original));
        let (retired, mut released) = walk_row(&mut w, &cells, 5, "ab    ");
        released.sort_unstable();
        released.dedup();
        assert!(retired.is_empty());
        assert_eq!(released, vec![(5, 2), (5, 3), (5, 4), (5, 5)]);
        assert!(w.released_cell_matches(&held, &original));
        assert!(!w.released_cell_matches(&cells[0], &original));
        assert!(!w.released_cell_matches(&held, &row("abxdef")));
        assert!(!w.released_cell_matches(&held, &row("ab    ")));
        let mut changed = held;
        changed.born += std::time::Duration::from_millis(1);
        assert!(!w.released_cell_matches(&changed, &original));
        changed = held;
        changed.row += 1;
        assert!(!w.released_cell_matches(&changed, &original));
        if text.contains('\0') {
            assert!(w.released_cell_matches(&cells[3], &original));
            assert!(!w.released_cell_matches(&held, &row("ab你def")));
            assert!(!w.released_cell_matches(&cells[3], &row("abc\0ef")));
        }
        let mut restored = w.clone();
        restored.restored(&[7], &cells);
        assert!(!restored.released_cell_matches(&held, &original));
        walk_row(&mut w, &cells, 5, "abxxxx");
        assert!(!w.released_cell_matches(&held, &original));
    }
}

#[test]
fn a_glyph_landing_on_a_blank_with_the_rewrite_that_took_its_run_goes_with_the_run() {
    let t0 = Instant::now();
    let mut w = Witness::new();
    let cells = [cell(5, 2, t0), cell(5, 3, t0), cell(5, 4, t0)];
    assert_eq!(walk_row(&mut w, &cells, 5, "xx cd"), (vec![], vec![]));
    assert_eq!(w.len(), 2, "premise: the Space is never armed");
    let (out, rel) = walk_row(&mut w, &cells, 5, "xxzqw");
    assert_eq!(out, vec![(5, 3), (5, 4), (5, 2)]);
    assert!(rel.is_empty());
    assert!(w.is_empty(), "no record is left for the melting cells");

    let t1 = t0 + std::time::Duration::from_millis(60);
    let mut w = Witness::new();
    let word = [cell(5, 3, t0), cell(5, 4, t0)];
    assert_eq!(walk_row(&mut w, &word, 5, "xxxcd"), (vec![], vec![]));
    let keyed = [cell(5, 2, t1), cell(5, 3, t0), cell(5, 4, t0)];
    let (out, _) = walk_row(&mut w, &keyed, 5, "xxzqw");
    assert_eq!(out, vec![(5, 3), (5, 4)], "the key's cell is not named");
    assert_eq!(w.len(), 1, "…and it is armed");

    let mut w = Witness::new();
    let line = [
        cell(5, 0, t0),
        cell(5, 1, t0),
        cell(5, 2, t0),
        cell(5, 3, t0),
        cell(5, 4, t0),
    ];
    assert_eq!(walk_row(&mut w, &line, 5, "ab cd"), (vec![], vec![]));
    assert_eq!(walk_row(&mut w, &line, 5, "abxcd"), (vec![], vec![]));
    assert_eq!(w.len(), 5, "the landed glyph is armed with its run");
}

#[test]
fn the_last_walk_s_blanks_ride_with_their_cells() {
    let t0 = Instant::now();
    for mover in 0..3 {
        let mut w = Witness::new();
        let mut cells = [cell(5, 2, t0), cell(5, 3, t0), cell(5, 4, t0)];
        assert_eq!(walk_row(&mut w, &cells, 5, "xx cd"), (vec![], vec![]));
        match mover {
            0 => {
                let mut moved: Vec<_> = cells.iter().map(|c| (c.row, c.col, c.born)).collect();
                w.translate_cells(&mut moved, -1);
            }
            1 => w.translate(1, 0),
            _ => w.translate_band(0, 10, -1),
        }
        for c in &mut cells {
            c.row = 4;
        }
        let (out, _) = walk_row(&mut w, &cells, 4, "xxzqw");
        assert_eq!(out, vec![(4, 3), (4, 4), (4, 2)], "mover {mover}");
    }
}

#[test]
fn a_released_record_is_kept_so_a_glyph_landing_under_it_later_melts_fast_and_counts_once() {
    let t0 = Instant::now();
    let cells = [cell(5, 0, t0), cell(5, 1, t0)];
    let release = |w: &mut Witness, out: &mut Vec<_>, rel: &mut Vec<_>| {
        for text in ["hi", "  "] {
            w.walk(
                &cells,
                &[RowSample {
                    row: 5,
                    cols: &row(text),
                }],
                &[],
                Instant::now(),
                out,
                rel,
            );
        }
    };
    let mut w = Witness::new();
    let (mut out, mut rel) = (Vec::new(), Vec::new());
    release(&mut w, &mut out, &mut rel);
    let n = w.walk(
        &cells,
        &[RowSample {
            row: 5,
            cols: &row("Th"),
        }],
        &[],
        Instant::now(),
        &mut out,
        &mut rel,
    );
    assert_eq!(
        pos(&out),
        vec![(5, 0), (5, 1)],
        "text under a released cell is REPLACED text: the melt"
    );
    assert!(
        out.iter().all(|&(_, _, born)| born == t0),
        "named by identity"
    );
    assert!(rel.is_empty(), "never released twice");
    assert_eq!(
        n, 2,
        "both are re-verdicts of cells already released and counted"
    );
    assert!(
        w.is_empty(),
        "a retired cell's record goes: it is stamped and leaving"
    );
    let mut w = Witness::new();
    release(&mut w, &mut out, &mut rel);
    let n = w.walk(
        &cells,
        &[RowSample {
            row: 5,
            cols: &row("hi"),
        }],
        &[],
        Instant::now(),
        &mut out,
        &mut rel,
    );
    assert!(
        out.is_empty() && rel.is_empty() && n == 0,
        "the same glyph touches nothing"
    );
    assert_eq!(w.len(), 2);
    let n = w.walk(
        &cells,
        &[
            RowSample {
                row: 5,
                cols: &row("  "),
            },
            RowSample {
                row: 7,
                cols: &row("hi"),
            },
        ],
        &[],
        Instant::now(),
        &mut out,
        &mut rel,
    );
    assert!(out.is_empty() && rel.is_empty() && n == 0);
    assert_eq!(w.len(), 2);
}

#[test]
fn a_released_run_s_never_armed_cell_is_held_with_it_and_a_glyph_on_it_melts_the_run() {
    let t0 = Instant::now();
    let mut w = Witness::new();
    let (mut out, mut rel) = (Vec::new(), Vec::new());
    let word: Vec<Cell> = (0..5u16).map(|c| cell_of(5, c, t0, 7)).collect();
    w.walk(
        &word,
        &[RowSample {
            row: 5,
            cols: &row("ab cd"),
        }],
        &[],
        Instant::now(),
        &mut out,
        &mut rel,
    );
    assert_eq!(w.len(), 4, "the four letters, not the blank");
    let n = w.walk(
        &word,
        &[RowSample {
            row: 5,
            cols: &row("     "),
        }],
        &[],
        Instant::now(),
        &mut out,
        &mut rel,
    );
    assert_eq!(rel.len(), 5, "the run released, the blank with it");
    assert_eq!(n, 0);
    assert_eq!(
        w.len(),
        5,
        "…and the blank now holds a released record of its own"
    );
    let n = w.walk(
        &word,
        &[RowSample {
            row: 5,
            cols: &row("  *  "),
        }],
        &[],
        Instant::now(),
        &mut out,
        &mut rel,
    );
    let mut got = pos(&out);
    got.sort_unstable();
    got.dedup();
    assert_eq!(got, vec![(5, 0), (5, 1), (5, 2), (5, 3), (5, 4)]);
    assert!(rel.is_empty());
    assert_eq!(n, 5, "every cell was counted when released: none again");
    assert!(w.is_empty());
}

#[test]
fn a_partial_release_takes_no_space_from_between_standing_letters() {
    let cols = |r: std::ops::Range<u16>| r.collect::<Vec<u16>>();
    for (armed, then, released) in [
        ("ab cd ef", "ab cd e ", vec![7]),
        ("ab cd ef", "ab c    ", cols(4..8)),
        ("ab cd ef", "        ", cols(0..8)),
        ("hello world ", "hello       ", cols(6..12)),
        (" hello world", "       world", cols(0..6)),
    ] {
        let (mut w, cells) = armed_line(armed);
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        let then_row = row(then);
        let sample = [RowSample {
            row: 5,
            cols: &then_row,
        }];
        w.walk(&cells, &sample, &[], Instant::now(), &mut out, &mut rel);
        assert!(
            out.is_empty(),
            "`{then}`: nothing replaced: {:?}",
            pos(&out)
        );
        let mut got: Vec<u16> = pos(&rel).into_iter().map(|(_, c)| c).collect();
        got.sort_unstable();
        got.dedup();
        assert_eq!(got, released, "`{armed}` -> `{then}`");
    }
}

#[test]
fn a_partial_release_of_one_glyph_leaves_the_run_s_spaces_standing() {
    let t0 = Instant::now();
    let t1 = t0 + std::time::Duration::from_millis(50);
    let word: Vec<Cell> = (0..11u16).map(|c| cell_of(5, c, t0, 7)).collect();
    let mut typed = word.clone();
    typed.push(cell_of(5, 11, t1, 7));
    let released = || {
        let mut w = Witness::new();
        walk_row(&mut w, &word, 5, "hello world");
        assert_eq!(w.len(), 10, "ten letters armed, not the space");
        assert_eq!(
            walk_row(&mut w, &word, 5, "hello worl "),
            (vec![], vec![(5, 10)]),
            "the one blanked glyph is released, and no space with it"
        );
        assert_eq!(w.len(), 10, "the space took no released record");
        assert_eq!(
            walk_row(&mut w, &typed, 5, "hello worl x"),
            (vec![], vec![])
        );
        assert_eq!(w.len(), 11, "the new key's cell is armed");
        w
    };
    let restore = |w: &Witness, cells: &[Cell], text: &str| {
        let mut runs = Vec::new();
        w.clone().restored_runs(
            cells,
            &[RowSample {
                row: 5,
                cols: &row(text),
            }],
            &mut runs,
        );
        runs
    };
    for (back, retired) in [("hello worldx", vec![]), ("hello worldy", vec![(5, 11)])] {
        let mut w = released();
        assert_eq!(
            restore(&w, &typed, back),
            vec![(7, None)],
            "`{back}`: the run is restorable with a key typed inside the melt"
        );
        w.restored(&[7], &typed);
        assert_eq!(
            walk_row(&mut w, &typed, 5, back),
            (retired, vec![]),
            "`{back}`: the row stands, bar a rewritten key's own cell"
        );
    }
    let mut w = released();
    let mut retyped = word.clone();
    retyped.push(cell_of(5, 10, t1, 7));
    walk_row(&mut w, &retyped, 5, "hello world");
    assert!(restore(&w, &retyped, "hello world").is_empty());
}

#[test]
fn a_relaid_run_melts_fast_and_a_vanished_or_unmatched_one_is_released() {
    type Case = (
        &'static str,
        &'static [(u16, &'static str)],
        &'static [u16],
        &'static [u16],
    );
    let cases: [Case; 11] = [
        ("hi", &[(5, "H ")], &[0, 1], &[]),
        ("hi", &[(5, "  "), (7, "hi")], &[0, 1], &[]),
        ("hi", &[(5, "    hi")], &[0, 1], &[]),
        ("hi", &[(5, "  "), (7, "> ok")], &[], &[0, 1]),
        ("hi", &[(5, "  "), (7, "h ho")], &[], &[0, 1]),
        ("z", &[(5, " "), (7, "zoom")], &[], &[0]),
        ("\u{4f60}\0", &[(5, "  "), (7, "\u{4f60}\0")], &[], &[0, 1]),
        ("ab TO cd TO", &[(5, "ab TO cd   ")], &[], &[9, 10]),
        ("ab cd", &[(5, "  x  ")], &[], &[0, 1, 2, 3, 4]),
        ("ab cd ef", &[(5, "ab*cd eX")], &[7], &[]),
        ("ab cd efgh", &[(5, "ab*cd egh ")], &[7, 8, 9], &[]),
    ];
    let t0 = Instant::now();
    for (armed, then, want_retire, want_release) in cases {
        let what = format!("`{armed}` -> {then:?}");
        let n = u16::try_from(armed.chars().count()).expect("a short line");
        let cells: Vec<Cell> = (0..n).map(|c| cell(5, c, t0)).collect();
        let mut w = Witness::new();
        assert_eq!(
            walk_row(&mut w, &cells, 5, armed),
            (vec![], vec![]),
            "{what}"
        );
        let rows: Vec<Vec<char>> = then.iter().map(|&(_, s)| row(s)).collect();
        let samples: Vec<RowSample<'_>> = then
            .iter()
            .zip(&rows)
            .map(|(&(r, _), cols)| RowSample { row: r, cols })
            .collect();
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        w.walk(&cells, &samples, &[], Instant::now(), &mut out, &mut rel);
        let named = |v: &[(u16, u16, Instant)]| {
            let mut got = pos(v);
            got.sort_unstable();
            got.dedup();
            got
        };
        let cols = |c: &[u16]| c.iter().map(|&c| (5, c)).collect::<Vec<_>>();
        assert_eq!(named(&out), cols(want_retire), "{what}: retired");
        assert_eq!(named(&rel), cols(want_release), "{what}: released");
    }
}

#[test]
fn a_different_glyph_retires_and_a_relaid_cell_arms_afresh() {
    let t0 = Instant::now();
    let mut w = Witness::new();
    let mut out = Vec::new();
    let mut rel = Vec::new();
    let cells = [cell(5, 0, t0)];
    w.walk(
        &cells,
        &[RowSample {
            row: 5,
            cols: &row("h"),
        }],
        &[],
        Instant::now(),
        &mut out,
        &mut rel,
    );
    w.walk(
        &cells,
        &[RowSample {
            row: 5,
            cols: &row("H"),
        }],
        &[],
        Instant::now(),
        &mut out,
        &mut rel,
    );
    assert_eq!(pos(&out), vec![(5, 0)], "h -> H is an overwrite");
    let later = t0 + std::time::Duration::from_millis(90);
    let relaid = [cell(5, 0, later)];
    w.walk(
        &relaid,
        &[RowSample {
            row: 5,
            cols: &row("x"),
        }],
        &[],
        Instant::now(),
        &mut out,
        &mut rel,
    );
    assert!(
        out.is_empty(),
        "the old record must not retire a re-laid cell"
    );
    assert_eq!(w.len(), 1);
    w.walk(
        &relaid,
        &[RowSample {
            row: 5,
            cols: &row("x"),
        }],
        &[],
        Instant::now(),
        &mut out,
        &mut rel,
    );
    assert!(out.is_empty());
}

#[test]
fn a_wide_glyph_s_two_cells_are_retired_as_one_unit() {
    let t0 = Instant::now();
    let mut w = Witness::new();
    let mut out = Vec::new();
    let mut rel = Vec::new();
    let cells = [cell(5, 3, t0), cell(5, 4, t0)];
    w.walk(
        &cells,
        &[RowSample {
            row: 5,
            cols: &row("   \u{4f60}\0"),
        }],
        &[],
        Instant::now(),
        &mut out,
        &mut rel,
    );
    assert!(out.is_empty() && w.len() == 2);
    w.walk(
        &cells,
        &[RowSample {
            row: 5,
            cols: &row("   ab"),
        }],
        &[],
        Instant::now(),
        &mut out,
        &mut rel,
    );
    let mut got = pos(&out);
    got.sort_unstable();
    got.dedup();
    assert_eq!(got, vec![(5, 3), (5, 4)]);
    let mut w = Witness::new();
    let cont = [cell(5, 4, t0)];
    w.walk(
        &cont,
        &[RowSample {
            row: 5,
            cols: &row("   \u{4f60}\0"),
        }],
        &[],
        Instant::now(),
        &mut out,
        &mut rel,
    );
    w.walk(
        &cont,
        &[RowSample {
            row: 5,
            cols: &row("     "),
        }],
        &[],
        Instant::now(),
        &mut out,
        &mut rel,
    );
    assert!(out.is_empty(), "blanked, and nowhere else: not the melt");
    assert_eq!(pos(&rel), vec![(5, 4), (5, 3)], "released as one unit");
    assert!(
        rel.iter().all(|&(_, _, born)| born == t0),
        "the partner is named with the SAME birth: one keystroke laid both"
    );
}

#[test]
fn a_stale_cell_under_a_fresh_one_is_named_by_its_own_birth_and_the_fresh_one_never() {
    let t0 = Instant::now();
    let t1 = t0 + std::time::Duration::from_millis(540);
    let mut out = Vec::new();
    let mut rel = Vec::new();
    for stale_first in [true, false] {
        let mut w = Witness::new();
        let stale = cell_of(5, 8, t0, 7);
        let blank = cell_of(5, 5, t0, 7);
        w.walk(
            &[blank, stale],
            &[RowSample {
                row: 5,
                cols: &row("hello world"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(
            out.is_empty() && w.len() == 1,
            "the `r` is armed, the blank not"
        );
        let fresh = cell_of(5, 8, t1, 9);
        let pool = if stale_first {
            [blank, stale, fresh]
        } else {
            [fresh, blank, stale]
        };
        w.walk(
            &pool,
            &[RowSample {
                row: 5,
                cols: &row("hello woXld"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        let mut got = out.clone();
        got.sort_unstable();
        assert_eq!(
            got,
            vec![(5, 5, t0), (5, 8, t0)],
            "stale first = {stale_first}: the stale cell and its run's blank, by birth — never the fresh cell"
        );
        assert_eq!(
            w.len(),
            1,
            "stale first = {stale_first}: the fresh cell's record stands alone"
        );
        w.walk(
            &[fresh],
            &[RowSample {
                row: 5,
                cols: &row("hello woXld"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty(), "stale first = {stale_first}");
    }
}

#[test]
fn standing_run_index_matches_the_retirement_oracle() {
    let t0 = Instant::now();
    let t1 = t0 + std::time::Duration::from_millis(1);
    let mut cells = [
        cell_of(2, 0, t0, 7),
        cell_of(2, 1, t0, 7),
        cell_of(2, 2, t0, 7),
        cell_of(3, 0, t0, 7),
        cell_of(3, 1, t0, 7),
        cell_of(3, 2, t0, 7),
        cell_of(5, 0, t0, 9),
        cell_of(5, 1, t0, 9),
        cell_of(6, 0, t0, 10),
        cell_of(2, 0, t1, 11),
    ];
    cells[7].retire_at = Some(t0);
    let mut witness = Witness::new();
    for i in [0, 2, 3, 4, 7, 9] {
        witness.arm(
            &cells[i],
            Unit {
                ch: if i == 3 || i == 4 { '你' } else { 'a' },
                wide: i == 3 || i == 4,
                cont: i == 4,
            },
            &[None; 4],
            t0,
        );
    }
    witness.hold_released(&cells[6]);
    let runs = [(5, 9), (2, 7), (6, 10), (3, 7), (2, 11)];

    let armed = [0, 2, 3, 4, 6, 7, 9];
    for mask in 0..(1usize << armed.len()) {
        let mut retire = Vec::new();
        for (bit, &i) in armed.iter().enumerate().rev() {
            if mask & (1 << bit) != 0 {
                let c = cells[i];
                retire.push((c.row, c.col, c.born));
                retire.push((c.row, c.col, c.born));
            }
        }
        let mut expected: Vec<_> = runs
            .iter()
            .copied()
            .filter(|&run| {
                cells.iter().any(|c| {
                    !c.leaving()
                        && (c.row, c.cohort) == run
                        && witness.find(c.row, c.col, c.born).is_some()
                        && !retire.contains(&(c.row, c.col, c.born))
                })
            })
            .collect();
        expected.sort_unstable();
        let original_order = retire.clone();
        witness.retired_runs.clear();
        witness.retired_runs.extend_from_slice(&runs);
        witness.find_standing_runs(&cells, &retire);
        assert_eq!(witness.standing_runs, expected, "mask {mask}");
        assert_eq!(retire, original_order, "verdict order must stay unchanged");
    }
    witness.retired_runs.clear();
    witness.find_standing_runs(&cells, &[]);
    assert!(
        witness.standing_runs.is_empty(),
        "no stale run state on an idle walk"
    );
}

#[test]
fn an_unsampled_row_keeps_its_records_and_a_dropped_cell_is_forgotten() {
    let t0 = Instant::now();
    let mut w = Witness::new();
    let mut out = Vec::new();
    let mut rel = Vec::new();
    let cells = [cell(5, 0, t0), cell(7, 0, t0)];
    w.walk(
        &cells,
        &[
            RowSample {
                row: 5,
                cols: &row("a"),
            },
            RowSample {
                row: 7,
                cols: &row("b"),
            },
        ],
        &[],
        Instant::now(),
        &mut out,
        &mut rel,
    );
    assert_eq!(w.len(), 2);
    w.walk(
        &cells,
        &[RowSample {
            row: 5,
            cols: &row("a"),
        }],
        &[],
        Instant::now(),
        &mut out,
        &mut rel,
    );
    assert!(out.is_empty() && w.len() == 2);
    w.walk(
        &cells[..1],
        &[RowSample {
            row: 5,
            cols: &row("a"),
        }],
        &[],
        Instant::now(),
        &mut out,
        &mut rel,
    );
    assert!(out.is_empty() && w.len() == 1);
}

#[test]
fn a_band_move_carries_the_records_inside_it_drops_what_leaves_and_keeps_the_list_sorted() {
    let t0 = Instant::now();
    let mut w = Witness::new();
    let mut out = Vec::new();
    let mut rel = Vec::new();
    let cells = [cell(2, 5, t0), cell(3, 1, t0), cell(5, 0, t0)];
    let glyphs = row("abcdef");
    let samples: Vec<RowSample<'_>> = [2u16, 3, 5]
        .iter()
        .map(|&r| RowSample {
            row: r,
            cols: &glyphs,
        })
        .collect();
    w.walk(&cells, &samples, &[], Instant::now(), &mut out, &mut rel);
    assert_eq!(w.len(), 3);
    w.translate_band(3, 5, -1);
    let moved = [cell(2, 5, t0), cell(2, 1, t0), cell(4, 0, t0)];
    let samples: Vec<RowSample<'_>> = [2u16, 4]
        .iter()
        .map(|&r| RowSample {
            row: r,
            cols: &glyphs,
        })
        .collect();
    w.walk(&moved, &samples, &[], Instant::now(), &mut out, &mut rel);
    assert!(
        out.is_empty() && w.len() == 3,
        "every record followed its cell and none was re-armed"
    );
    let blank = row("a     ");
    w.walk(
        &moved,
        &[
            RowSample {
                row: 2,
                cols: &blank,
            },
            RowSample {
                row: 4,
                cols: &glyphs,
            },
        ],
        &[],
        Instant::now(),
        &mut out,
        &mut rel,
    );
    assert_eq!(pos(&out), vec![(2, 1), (2, 5)]);
    assert!(rel.is_empty());
    let mut w = Witness::new();
    w.walk(
        &[cell(3, 0, t0)],
        &[RowSample {
            row: 3,
            cols: &glyphs,
        }],
        &[],
        Instant::now(),
        &mut out,
        &mut rel,
    );
    w.translate_band(3, 5, -1);
    assert!(w.is_empty(), "carried past the band's edge: gone");
}

#[test]
fn the_witness_is_bounded_and_allocates_nothing_past_its_cap() {
    let t0 = Instant::now();
    let mut w = Witness::new();
    let cap = w.seen.capacity();
    let mut out = Vec::new();
    let mut rel = Vec::new();
    let cells: Vec<Cell> = (0..(WITNESS_CAP as u16 + 40))
        .map(|i| cell(i / 200, i % 200, t0))
        .collect();
    let glyphs: Vec<char> = vec!['x'; 200];
    let samples: Vec<RowSample<'_>> = (0..6)
        .map(|r| RowSample {
            row: r,
            cols: &glyphs,
        })
        .collect();
    w.walk(&cells, &samples, &[], Instant::now(), &mut out, &mut rel);
    assert_eq!(w.len(), WITNESS_CAP);
    assert_eq!(w.seen.capacity(), cap, "no growth past the reserved cap");
}

#[test]
fn the_deferral_s_scratch_is_reserved_and_never_grows() {
    let t0 = Instant::now();
    let mut w = Witness::new();
    let caps = (w.blind_changed.capacity(), w.deferred_runs.capacity());
    assert!(
        caps.0 >= WITNESS_CAP && caps.1 >= WITNESS_CAP,
        "reserved: {caps:?}"
    );
    let cap = u16::try_from(WITNESS_CAP).expect("a small cap");
    let cells: Vec<Cell> = (0..cap)
        .map(|i| cell_of(2 * (i / 200), i % 200, t0, u32::from(i / 2)))
        .collect();
    fn even(cols: &[char]) -> Vec<RowSample<'_>> {
        (0..6).map(|k| RowSample { row: 2 * k, cols }).collect()
    }
    let was: Vec<char> = vec!['x'; 200];
    let changed: Vec<char> = vec!['y'; 200];
    let (mut out, mut rel) = (Vec::new(), Vec::new());
    w.walk(&cells, &even(&was), &[], t0, &mut out, &mut rel);
    assert_eq!(w.len(), WITNESS_CAP, "fixture: a full witness");
    let odd: Vec<u16> = (0..6).map(|k| 2 * k + 1).collect();
    let later = t0 + Duration::from_millis(16);
    w.walk(&cells, &even(&changed), &odd, later, &mut out, &mut rel);
    assert!(out.is_empty() && rel.is_empty(), "every run waits a walk");
    assert_eq!(w.blind_changed.len(), WITNESS_CAP, "every record read");
    assert_eq!(w.deferred_runs.len(), WITNESS_CAP / 2, "every run deferred");
    assert_eq!(
        (w.blind_changed.capacity(), w.deferred_runs.capacity()),
        caps,
        "no growth past the reserved capacity"
    );
}

#[test]
fn awaiting_and_last_walk_follow_scroll_band_and_clear() {
    let t0 = Instant::now();
    let mut w = Witness::new();
    let (mut out, mut rel) = (Vec::new(), Vec::new());
    w.walk(
        &[cell(5, 0, t0)],
        &[RowSample {
            row: 5,
            cols: &['a'],
        }],
        &[],
        t0,
        &mut out,
        &mut rel,
    );
    assert_eq!(w.last_walk, Some(t0));

    w.await_caret(Some((5, 7)));
    w.translate(2, 10);
    assert_eq!(w.awaiting, Some((3, 7)));

    w.translate_band(2, 6, 1);
    assert_eq!(w.awaiting, Some((4, 7)));

    w.translate_band(2, 6, -3);
    assert_eq!(w.awaiting, None);

    w.clear();
    assert_eq!(w.last_walk, None);
    assert_eq!(w.awaiting, None);
}
