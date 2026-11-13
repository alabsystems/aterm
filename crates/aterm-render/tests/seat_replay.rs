// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// The one seat rule beside the seat on glass: the trail gallery's scenes replayed through the
// shipping effects pipeline and the real renderer (`companion_off_letters`' replays), and on
// every frame's page the seat `aterm_effects::pet_seat::decide` would choose, read beside the
// box `PetBrain` drew. Nothing draws from the rule yet; this reads where the two differ.
//
// It compares SEATS ONLY. The old side is a drawn box, in motion between its seats; the rule's
// side is a whole-cell seat with no frame between two seats, so "the rule's seat is on no
// letter" holds by construction and says nothing of a walk or a leap. The like-for-like columns
// are the old box on letters AT REST (the box where the frame before had it) and each side's
// alpha dips: for the rule, every yield and every way that is a cut, which the package's exit
// test (`alpha_dip = 0`) counts against it.

use crate::companion_off_letters::{Drive, Played, coats_driven, flood_at, last_row_at, walled};
use crate::trail_letters_read::{Coat, DARK, Judged, Scene, gallery_cell};
use aterm_effects::kitty_pet::{ART_ROWS, art_cols, head_law};
use aterm_effects::pet_seat::{
    Body, Edge, Page, Path, Rule, Scene as SeatScene, Seat, SeatState, Why, decide, legal, path,
};

/// The pets a replay seats: the cat and the dog, by the styles that draw them today.
const PETS: [(&str, &str); 2] = [("cat", "rainbow + cat"), ("dog", "rainbow + dog")];

/// A replay the reader runs: its name in the table, and whether its page is a full-screen
/// program's ([`Rule::FULL_SCREEN`]).
enum Run {
    Gallery(Scene),
    LastRow(u16),
    Flood(u16),
}

impl Run {
    fn name(&self) -> String {
        match self {
            Run::Gallery(scene) => format!("{scene:?}"),
            Run::LastRow(cols) => format!("last-row at {cols}"),
            Run::Flood(cols) => format!("flood at {cols}"),
        }
    }

    fn full_screen(&self) -> bool {
        matches!(self, Run::Gallery(Scene::FocusWrap))
    }

    fn judged(&self, style: &str) -> Vec<Judged> {
        let played = |(ev, tail): Played| (ev, tail);
        match self {
            Run::Gallery(scene) => coats_driven(style, DARK, *scene, &[], Drive::Console),
            Run::LastRow(cols) => {
                let (p, _) = last_row_at(*cols);
                walled((style, DARK), *cols, Drive::Console, &played(p), &[])
            }
            Run::Flood(cols) => {
                let (p, _) = flood_at(*cols);
                walled((style, DARK), *cols, Drive::Console, &played(p), &[])
            }
        }
    }
}

/// A frame's page as the seat rule reads it: the letters, and the cells a word icon lies on.
struct FramePage<'a>(&'a Coat);

impl Page for FramePage<'_> {
    fn rows(&self) -> u16 {
        self.0.lettered.len() as u16
    }

    fn cols(&self) -> u16 {
        self.0.lettered.first().map_or(0, Vec::len) as u16
    }

    fn lettered(&self, row: u16, col: u16) -> bool {
        let (r, c) = (f32::from(row), f32::from(col));
        self.0.lettered[usize::from(row)][usize::from(col)]
            || self
                .0
                .icon_cells
                .iter()
                .any(|&(x0, x1, y0, y1)| c < x1 && c + 1.0 > x0 && r < y1 && r + 1.0 > y0)
    }
}

/// A box on the pane's grid, in cells from its first: `(col0, col1, row0, row1)`, end exclusive.
type Cells = (f32, f32, f32, f32);

/// What one replay's frames sum to, the drawn seat's and the rule's.
#[derive(Debug, Default)]
struct Sum {
    frames: usize,
    /// PetBrain's: frames with the body on glass; of them, frames whose box meets a lettered
    /// cell by a pixel or more (`aterm-lab trail-companion`'s `on_letters`) and frames whose box
    /// lies on the caret's cell; and how often it went off the glass and came back.
    old_on: usize,
    old_letters: usize,
    old_caret: usize,
    old_returns: usize,
    /// Of its frames on glass, those with the box where the frame before had it (at rest), and
    /// of those the frames it meets a lettered cell: the seat PetBrain chose, without the way
    /// to it.
    old_rest: usize,
    old_letters_rest: usize,
    /// How often its alpha began to fall (a frame drawn under the frame before's).
    old_dips: usize,
    /// How long (ms) the shortest of its stays off the glass between two on it lasted.
    old_shortest_away: Option<u64>,
    /// The rule's: frames with a seat on the page; changes of seat by their reason; the ways
    /// between them; and the shortest stay off the glass.
    new_on: usize,
    yielded: usize,
    /// Of the yields, those to a word icon with no glyph under the seat.
    icon_yields: usize,
    gave_way: usize,
    switched: usize,
    moved: usize,
    returned: usize,
    /// The changes stage 2 would draw as an alpha dip: a yield, or a way that is a cut.
    new_dips: usize,
    walks: usize,
    leaps: usize,
    exits: usize,
    enters: usize,
    cuts: usize,
    new_shortest_away: Option<u64>,
    /// The scrolls its seat rode.
    rides: usize,
    /// Both on glass: the frames their boxes share a cell, and the frames they share none.
    together: usize,
    apart: usize,
    /// One on glass and the other off it.
    only_old: usize,
    only_new: usize,
    /// The first frames (ms) of each kind of difference, to look at.
    first_apart: Option<(u64, Cells, Seat)>,
    first_only_old: Option<u64>,
    first_only_new: Option<(u64, Seat)>,
}

/// The cells `(col0, col1, row0, row1)` a seat's box takes, end exclusive.
fn seat_cells(body: Body, seat: Seat) -> Option<Cells> {
    let Seat::On { row, col, small } = seat else {
        return None;
    };
    let size = if small { body.small? } else { body.whole };
    Some((
        f32::from(col),
        f32::from(col + size.cols),
        f32::from(row) + 1.0 - f32::from(size.rows),
        f32::from(row) + 1.0,
    ))
}

fn shorter(least: &mut Option<u64>, ms: u64) {
    *least = Some(least.map_or(ms, |l| l.min(ms)));
}

/// The seat rule run over a replay's frames beside the drawn body.
///
/// The rule's inputs the replay has no reader for are stood in for here: the rows about the
/// caret (`Rule::reads`) are quiet since their letters or the caret last changed; the writing
/// is 3 cells ahead while the caret stepped forward along its row within the last 300 ms, and 0
/// otherwise; the next line prints under the caret's row except on a full-screen page; nothing
/// holds the pet; no page is said to be another page; and a page whose rows are the frame
/// before's one to three rows up is read as scrolled (`SeatState::ride`).
fn seated(judged: &[Judged], full_screen: bool) -> Sum {
    // `SEAT_TRACE=<ms>`: a line for each change of seat and one every so many ms.
    let trace: u64 = std::env::var("SEAT_TRACE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let (cw, ch) = gallery_cell();
    let body = Body::of_art(
        art_cols(cw as u16, ch as u16),
        ART_ROWS,
        head_law(true, true).1,
    );
    let rule = if full_screen {
        Rule::FULL_SCREEN
    } else {
        Rule::DEFAULT
    };
    let mut sum = Sum::default();
    let mut state = SeatState::offstage(Edge::Right);
    let (mut before, mut before2): (Option<&Judged>, Option<&Judged>) = (None, None);
    let (mut changed_at, mut stepped_at) = (None::<u64>, None::<u64>);
    let (mut old_seen, mut old_off_at) = (false, None::<u64>);
    let (mut new_seen, mut new_off_at) = (false, None::<u64>);
    for j in judged {
        let c = &j.coat;
        if c.lettered.is_empty() {
            continue;
        }
        let page = FramePage(c);
        let caret = (c.caret_cell.0 as u16, c.caret_cell.1 as u16);
        if caret.0 >= page.rows() || caret.1 >= page.cols() {
            // A caret past the row's last cell (a line typed to its end, the wrap pending).
            continue;
        }
        let dt = before.map_or(16, |b| j.ms - b.ms) as u32;
        if let Some(b) = before {
            let (was, is) = (b.coat.caret_cell, c.caret_cell);
            let (first, last) = rule.reads(body, caret.0, page.rows());
            let read = usize::from(first)..=usize::from(last);
            if b.coat.lettered.get(read.clone()) != c.lettered.get(read) || was != is {
                changed_at = Some(j.ms);
            }
            if b.coat.lettered != c.lettered || was != is {
                // A page whose rows are the last frame's a few rows up scrolled: the seat rides it.
                let rows = c.lettered.len();
                if let Some(up) = (1..rows.min(4)).find(|&up| {
                    b.coat.lettered != c.lettered
                        && b.coat.lettered[up..] == c.lettered[..rows - up]
                        && b.coat.lettered[up..].iter().flatten().any(|&l| l)
                }) {
                    state = state.ride(up as i32, page.rows(), page.cols());
                    sum.rides += 1;
                }
            }
            if is.0 == was.0 && is.1 > was.1 && is.1 <= was.1 + 2 {
                stepped_at = Some(j.ms);
            }
        }
        let scene = SeatScene {
            page: &page,
            caret,
            body,
            ahead: if stepped_at.is_some_and(|at| j.ms - at <= 300) {
                3
            } else {
                0
            },
            below_is_next: !full_screen,
            quiet_ms: changed_at.map_or(60_000, |at| (j.ms - at) as u32),
            hold: false,
            new_page: false,
            scrolled: false,
        };
        let d = decide(&rule, &scene, state, dt);
        assert!(
            legal(&rule, &scene, d.state.seat),
            "{} ms: {:?} is no seat",
            j.ms,
            d.state.seat
        );
        if let Some((x0, x1, y0, y1)) = seat_cells(body, d.state.seat) {
            for row in (y0.max(0.0) as u16)..(y1 as u16) {
                for col in (x0 as u16)..(x1 as u16) {
                    assert!(
                        !page.lettered(row, col) && (row, col) != caret,
                        "{} ms: {:?} covers ({row}, {col})",
                        j.ms,
                        d.state.seat
                    );
                }
            }
        }
        match d.why {
            Why::Kept => {}
            Why::Yielded => {
                sum.yielded += 1;
                // A yield with no glyph under the seat it left is to a word icon alone: the icons
                // on glass keep off the drawn body's box, not the rule's.
                let glyph = seat_cells(body, state.seat).is_some_and(|(x0, x1, y0, y1)| {
                    ((y0.max(0.0) as usize)..(y1 as usize)).any(|row| {
                        ((x0 as usize)..(x1 as usize))
                            .any(|col| c.lettered[row].get(col).copied().unwrap_or(true))
                    })
                });
                sum.icon_yields += usize::from(!glyph);
            }
            Why::GaveWay => sum.gave_way += 1,
            Why::Switched => sum.switched += 1,
            Why::Moved => sum.moved += 1,
            Why::Returned => sum.returned += 1,
        }
        if d.why != Why::Kept {
            let way = path(&rule, &scene, state.seat, d.state.seat);
            match way {
                Path::Stay => {}
                Path::Walk => sum.walks += 1,
                Path::Leap => sum.leaps += 1,
                Path::Exit(_) => sum.exits += 1,
                Path::Enter(_) => sum.enters += 1,
                Path::Cut => sum.cuts += 1,
            }
            sum.new_dips += usize::from(d.why == Why::Yielded || way == Path::Cut);
        }
        state = d.state;

        sum.frames += 1;
        let old = c.pet_cells.filter(|_| c.alpha > 0);
        let new = seat_cells(body, state.seat);
        if old.is_some() {
            sum.old_on += 1;
            sum.old_letters += usize::from(!c.box_cells.is_empty());
            sum.old_caret += usize::from(c.box_on_caret);
            if before.is_some_and(|b| b.coat.alpha > 0 && b.coat.pet_cells == c.pet_cells) {
                sum.old_rest += 1;
                sum.old_letters_rest += usize::from(!c.box_cells.is_empty());
            }
            if let Some(at) = old_off_at.take() {
                sum.old_returns += 1;
                shorter(&mut sum.old_shortest_away, j.ms - at);
            }
            old_seen = true;
        } else if old_seen && old_off_at.is_none() {
            old_off_at = Some(j.ms);
        }
        if let (Some(b), Some(bb)) = (before, before2)
            && c.alpha < b.coat.alpha
            && b.coat.alpha >= bb.coat.alpha
        {
            sum.old_dips += 1;
        }
        if new.is_some() {
            sum.new_on += 1;
            if let Some(at) = new_off_at.take() {
                shorter(&mut sum.new_shortest_away, j.ms - at);
            }
            new_seen = true;
        } else if new_seen && new_off_at.is_none() {
            new_off_at = Some(j.ms);
        }
        if trace > 0 && (d.why != Why::Kept || j.ms / trace != before.map_or(0, |b| b.ms / trace)) {
            println!(
                "  {:6} ms caret {caret:?} old {} alpha {} on {:?} | new {:?} {:?}",
                j.ms,
                old.map_or_else(
                    || "off".to_string(),
                    |(x0, x1, y0, y1)| format!("cols {x0:.1}..{x1:.1} rows {y0:.2}..{y1:.2}")
                ),
                c.alpha,
                c.box_cells,
                state.seat,
                d.why,
            );
        }
        match (old, new) {
            (Some(o), Some(n)) => {
                if o.0 < n.1 && o.1 > n.0 && o.2 < n.3 && o.3 > n.2 {
                    sum.together += 1;
                } else {
                    sum.apart += 1;
                    sum.first_apart.get_or_insert((j.ms, o, state.seat));
                }
            }
            (Some(_), None) => {
                sum.only_old += 1;
                sum.first_only_old.get_or_insert(j.ms);
            }
            (None, Some(_)) => {
                sum.only_new += 1;
                sum.first_only_new.get_or_insert((j.ms, state.seat));
            }
            (None, None) => {}
        }
        before2 = before;
        before = Some(j);
    }
    sum
}

fn ms_or_dash(ms: Option<u64>) -> String {
    ms.map_or_else(|| "-".to_string(), |ms| ms.to_string())
}

/// The table's row for one replay.
fn row(name: &str, pet: &str, s: &Sum) -> String {
    format!(
        "| {name} | {pet} | {} | {} | {} | {}/{} | {} | {} | {} | {} | {} | {}/{}/{}/{}/{} | {}/{}/{}/{}/{} | {} | {} | {} | {} | {} | {} |",
        s.frames,
        s.old_on,
        s.old_letters,
        s.old_letters_rest,
        s.old_rest,
        s.old_caret,
        s.old_dips,
        s.old_returns,
        ms_or_dash(s.old_shortest_away),
        s.new_on,
        s.yielded,
        s.gave_way,
        s.switched,
        s.moved,
        s.returned,
        s.walks,
        s.leaps,
        s.exits,
        s.enters,
        s.cuts,
        s.new_dips,
        ms_or_dash(s.new_shortest_away),
        s.together,
        s.apart,
        s.only_old,
        s.only_new,
    )
}

const HEAD: &str = "\
| scene | pet | frames | old on glass | old on letters (in motion too) | old on letters at rest / frames at rest | old on caret | old alpha dips | old returns | old shortest away ms | new seat on page | new yield/give way/switch/move/return | new walk/leap/exit/enter/cut | new alpha dips (yields and cuts) | new shortest away ms | both, sharing a cell | both, apart | only old | only new |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |";

/// THE REPLAY COMPARISON, printed: every gallery scene, the last-row wall at 70 to 73 columns
/// and the flood at 70 and 72, for the cat and the dog. `SEAT_ONLY=<text>` keeps the rows whose
/// `scene/pet` holds it.
#[test]
#[ignore = "a survey: run by hand to read the one seat rule beside the seat on glass"]
fn survey_the_one_seat_rule_beside_the_seat_on_glass() {
    let only = std::env::var("SEAT_ONLY").unwrap_or_default();
    let mut runs: Vec<Run> = [
        Scene::Typing,
        Scene::EnterBottom,
        Scene::Burst,
        Scene::Cjk,
        Scene::MoveUp,
        Scene::SplitBurst,
        Scene::Context,
        Scene::Wrapped,
        Scene::FoldRest,
        Scene::FocusWrap,
        Scene::StatusWrap,
        Scene::LastRow,
        Scene::Steam,
    ]
    .into_iter()
    .map(Run::Gallery)
    .collect();
    runs.extend([70, 71, 72, 73].map(Run::LastRow));
    runs.extend([70, 72].map(Run::Flood));
    println!("{HEAD}");
    let mut notes = Vec::new();
    for run in &runs {
        for (pet, style) in PETS {
            let name = run.name();
            if !only.is_empty() && !format!("{name}/{pet}").contains(&only) {
                continue;
            }
            let s = seated(&run.judged(style), run.full_screen());
            println!("{}", row(&name, pet, &s));
            notes.push(format!(
                "{name}/{pet}: rides {}; yields to an icon alone {}; first apart {:?}; first only old {:?}; first only new {:?}",
                s.rides, s.icon_yields, s.first_apart, s.first_only_old, s.first_only_new
            ));
        }
    }
    for n in notes {
        println!("{n}");
    }
}

/// KI-1107-4's page, replayed: on the trail gallery's `last-row` at 71 columns the rule's pet is
/// never on a letter or the caret (asserted on every frame by [`seated`]), and leaves the glass
/// and comes back only on its feet through an edge: no letter lands under it and no way is a cut.
#[test]
fn at_the_71_column_wall_the_rule_s_pet_leaves_and_returns_on_its_feet() {
    let (played, _) = last_row_at(71);
    let judged = walled(("rainbow + cat", DARK), 71, Drive::Console, &played, &[]);
    let s = seated(&judged, false);
    assert!(s.frames > 500 && s.new_on > 100, "{s:?}");
    assert!(s.exits >= 1, "it walks off at the wall: {s:?}");
    assert_eq!((s.cuts, s.yielded, s.new_dips), (0, 0, 0), "{s:?}");
}
