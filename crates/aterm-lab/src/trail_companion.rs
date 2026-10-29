// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `trail-companion <path>...`: the companion over the letters, read off a trail gallery's sidecars, as JSON.
//!
//! Each frame `trail-gallery` photographs keeps its `trail status` row (`<stem>.<frame>.status.txt`), whose
//! `pet_body=X0,X1,Y0,Y1` names the companion's drawn body in window pixels (end exclusive), whose `head_body=` names
//! the flying head's the same way (looks10 L10: nyan's kitty, earned at the ribbon's head) and, from this verb's
//! round on, whose `word_icons=` names the word icons on glass the same way. The verb reads each box against every
//! lettered cell of the frame's screen text (`.text.txt`) on the grid of its dims reply (the frame's own
//! `<stem>.<frame>.dims.txt`, as a settings page's cursor scene card keeps, else `<stem>.dims.txt`), the
//! caret's cell (`.cursor.txt`) and each word icon, and counts the frames whose body meets any of them: the check
//! looks6 to looks8's judges ran by hand (`compcheck.py`, `petover.py`), now a tool.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::instance::{take_value, take_values};
use crate::paths::from_caller;
use crate::pyjson::J;
use crate::trail_contrast::{
    Glyph, Grid, PxBox, caret_cell, compact, frame_dims, grid_from_dims, read_text, round_to,
    screen_cells, sidecar,
};
use crate::{Fail, Res};

/// The share of a cell each side of a lettered cell is shaved by for the lenient count (`shaved`): the body's box
/// is its sprite's bounds, and a box that only grazes a cell's corner by less than this leaves its letter clear.
pub const SHAVE: f64 = 0.3;

/// The verb's own help (`trail-companion --help`).
pub const HELP: &str = "\
usage: targo --unverified lab trail-companion <path>... [--shave SHARE] [--max N] [--list all|hits]

The companion over the letters, as JSON: every frame under each <path> (a trail-gallery directory, walked, or one
frame's <stem>.<frame>.png) whose `trail status` sidecar (<stem>.<frame>.status.txt) names the companion's drawn body
(pet_body=X0,X1,Y0,Y1, window px, end exclusive) or the flying head's (head_body=, the same; a status without the
field is counted in heads_unread), each read against every lettered cell of the frame's screen text
(<stem>.<frame>.text.txt) on the grid of its dims reply (<stem>.<frame>.dims.txt, else <stem>.dims.txt), the caret's
cell (<stem>.<frame>.cursor.txt) and each word icon on glass the status names (word_icons=X0,X1,Y0,Y1;... or none; a
status without the field is counted in icons_unread). A frame counts in on_letters when its body meets a lettered cell by a pixel or more, in
shaved when it meets one shaved by SHARE of a cell on each side (default 0.3), in on_caret when it meets the caret's
cell and in on_icons when it meets a word icon, by either body; companion_over_letters counts the frames that do any
of the three, with_head the frames with a head on glass and head_over those whose head does.
--list hits (the default) lists those frames, all lists every frame read.

exit: 0; 1 companion_over_letters is over --max N; 2 a usage, file or parse error
";

/// A box in window pixels, end exclusive, in `trail status`'s order: `(x0, x1, y0, y1)`.
pub type Span = (i64, i64, i64, i64);

/// The box a `key=X0,X1,Y0,Y1` field of a `trail status` row names; `Ok(None)` for `none` or no such field.
/// # Errors
/// A field whose value is neither `none` nor four integers.
pub fn status_box(status: &str, key: &str) -> Result<Option<Span>, String> {
    match status_field(status, key) {
        None | Some("none") => Ok(None),
        Some(v) => parse_span(v, key).map(Some),
    }
}

/// The boxes a `key=X0,X1,Y0,Y1;...` field names (`none`: no box); `Ok(None)` when the row has no such field.
/// # Errors
/// A box that is not four integers.
pub fn status_boxes(status: &str, key: &str) -> Result<Option<Vec<Span>>, String> {
    match status_field(status, key) {
        None => Ok(None),
        Some("none" | "") => Ok(Some(Vec::new())),
        Some(v) => v
            .split(';')
            .map(|b| parse_span(b, key))
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
    }
}

/// The value of a `key=value` field of a `trail status` row.
pub(crate) fn status_field<'a>(status: &'a str, key: &str) -> Option<&'a str> {
    status
        .split_whitespace()
        .find_map(|t| t.strip_prefix(key)?.strip_prefix('='))
}

fn parse_span(v: &str, key: &str) -> Result<Span, String> {
    let n: Vec<i64> = v
        .split(',')
        .map(|p| p.trim().parse::<i64>())
        .collect::<Result<_, _>>()
        .map_err(|_| format!("{key}={v}: not X0,X1,Y0,Y1"))?;
    match n.as_slice() {
        &[x0, x1, y0, y1] => Ok((x0, x1, y0, y1)),
        _ => Err(format!("{key}={v}: not X0,X1,Y0,Y1")),
    }
}

/// The pixels two boxes share.
#[must_use]
pub fn overlap(a: Span, b: Span) -> i64 {
    let w = a.1.min(b.1) - a.0.max(b.0);
    let h = a.3.min(b.3) - a.2.max(b.2);
    if w > 0 && h > 0 { w * h } else { 0 }
}

/// A cell's box as a [`Span`].
#[must_use]
pub const fn cell_span(b: PxBox) -> Span {
    (b.x0, b.x1, b.y0, b.y1)
}

/// What one frame's companion meets.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Meets {
    /// The lettered cells its body meets by a pixel or more: `(row, col, glyph)`.
    pub letters: Vec<(i64, i64, char)>,
    /// The pixels it shares with those cells.
    pub letter_px: i64,
    /// The lettered cells it meets once each is shaved by the share on each side.
    pub shaved: usize,
    /// The pixels it shares with the caret's cell.
    pub caret_px: i64,
    /// The pixels it shares with the word icons.
    pub icon_px: i64,
}

impl Meets {
    /// Whether the body lies on a letter, the caret or an icon.
    #[must_use]
    pub const fn over(&self) -> bool {
        !self.letters.is_empty() || self.caret_px > 0 || self.icon_px > 0
    }
}

/// `body` against the `glyphs` on `grid` (blank glyphs are no letters), the `caret` cell and the `icons`.
#[must_use]
pub fn judge(
    body: Span,
    grid: &Grid,
    glyphs: &[Glyph],
    caret: Option<(i64, i64)>,
    icons: &[Span],
    shave: f64,
) -> Meets {
    let mut m = Meets::default();
    let (sx, sy) = (
        (shave * grid.cw as f64).round() as i64,
        (shave * grid.ch as f64).round() as i64,
    );
    for g in glyphs {
        if g.ch.is_whitespace() || !grid.holds(g.row, g.col) {
            continue;
        }
        let cell = cell_span(grid.cell_box(g.row, g.col, g.width));
        let px = overlap(body, cell);
        if px == 0 {
            continue;
        }
        m.letters.push((g.row, g.col, g.ch));
        m.letter_px += px;
        if overlap(body, (cell.0 + sx, cell.1 - sx, cell.2 + sy, cell.3 - sy)) > 0 {
            m.shaved += 1;
        }
    }
    if let Some((row, col)) = caret
        && grid.holds(row, col)
    {
        m.caret_px = overlap(body, cell_span(grid.cell_box(row, col, 1)));
    }
    m.icon_px = icons.iter().map(|&i| overlap(body, i)).sum();
    m
}

/// One frame read: its bodies, what each meets, and whether its status named the word icons and the head.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameRead {
    /// The frame's path.
    pub png: PathBuf,
    /// The companion's drawn body (`None`: not on glass).
    pub body: Option<Span>,
    /// What the body meets (empty without a body).
    pub meets: Meets,
    /// The flying head's drawn body (`None`: not on glass, or not named).
    pub head: Option<Span>,
    /// What the head meets (empty without a head).
    pub head_meets: Meets,
    /// Whether the status named the word icons.
    pub icons_read: bool,
    /// Whether the status named the head (`head_body=`).
    pub head_read: bool,
}

impl FrameRead {
    /// Whether either body is on glass.
    #[must_use]
    pub const fn bodied(&self) -> bool {
        self.body.is_some() || self.head.is_some()
    }

    /// Whether either body lies on a letter, the caret or an icon.
    #[must_use]
    pub const fn over(&self) -> bool {
        self.meets.over() || self.head_meets.over()
    }
}

/// Read the frame at `png` from its sidecars; `Ok(None)` when it has no `trail status` sidecar.
/// # Errors
/// A sidecar that cannot be read or parsed, or a body with no dims reply to place the cells.
pub fn read_frame(png: &Path, shave: f64) -> Result<Option<FrameRead>, String> {
    let Some(status_path) = sidecar(png, ".status.txt", true) else {
        return Ok(None);
    };
    let status = read_text(&status_path)?;
    let at = |e: String| format!("{}: {e}", status_path.display());
    let body = status_box(&status, "pet_body").map_err(at)?;
    let head = status_box(&status, "head_body").map_err(at)?;
    let head_read = status_field(&status, "head_body").is_some();
    let icons = status_boxes(&status, "word_icons").map_err(at)?;
    let icons_read = icons.is_some();
    if body.is_none() && head.is_none() {
        return Ok(Some(FrameRead {
            png: png.to_path_buf(),
            body: None,
            meets: Meets::default(),
            head: None,
            head_meets: Meets::default(),
            icons_read,
            head_read,
        }));
    }
    let dims = frame_dims(png).ok_or_else(|| {
        format!(
            "{}: no <stem>.<frame>.dims.txt or <stem>.dims.txt to place its cells",
            png.display()
        )
    })?;
    let grid = grid_from_dims(&read_text(&dims)?)?;
    let glyphs = match sidecar(png, ".text.txt", true) {
        Some(p) => {
            let lines: Vec<String> = read_text(&p)?.split('\n').map(str::to_string).collect();
            screen_cells(&lines)
        }
        None => Vec::new(),
    };
    let (caret, _) = caret_cell(None, sidecar(png, ".cursor.txt", true))?;
    let icons = icons.as_deref().unwrap_or(&[]);
    let meets_of = |b: Option<Span>| {
        b.map_or_else(Meets::default, |b| {
            judge(b, &grid, &glyphs, caret, icons, shave)
        })
    };
    Ok(Some(FrameRead {
        png: png.to_path_buf(),
        body,
        meets: meets_of(body),
        head,
        head_meets: meets_of(head),
        icons_read,
        head_read,
    }))
}

/// Every frame under `path`: the path itself when it is a frame, else each `<stem>.<frame>.png` (not a clean twin)
/// in the tree below it, in name order.
/// # Errors
/// A directory that cannot be read.
pub fn frames_under(path: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = BTreeSet::new();
    let mut todo = vec![path.to_path_buf()];
    while let Some(p) = todo.pop() {
        if p.is_dir() {
            let rd =
                std::fs::read_dir(&p).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
            for entry in rd.flatten() {
                todo.push(entry.path());
            }
        } else if is_frame(&p) {
            out.insert(p);
        }
    }
    Ok(out.into_iter().collect())
}

/// A gallery frame: `<stem>.<frame>.png`, not its clean twin.
fn is_frame(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".png"))
        .is_some_and(|base| base.contains('.') && !base.ends_with(".clean"))
}

fn span_json(s: Span) -> J {
    J::Arr(vec![J::Int(s.0), J::Int(s.1), J::Int(s.2), J::Int(s.3)])
}

fn cells_json(m: &Meets) -> J {
    let mut cells: Vec<J> = m
        .letters
        .iter()
        .take(8)
        .map(|&(r, c, g)| J::Arr(vec![J::Int(r), J::Int(c), J::s(g.to_string())]))
        .collect();
    if m.letters.len() > 8 {
        cells.push(J::s(format!("+{}", m.letters.len() - 8)));
    }
    J::Arr(cells)
}

fn frame_json(f: &FrameRead, root: &Path) -> J {
    let name = f
        .png
        .strip_prefix(root)
        .unwrap_or(&f.png)
        .to_string_lossy()
        .into_owned();
    let (m, h) = (&f.meets, &f.head_meets);
    let count = |n: usize| J::Int(i64::try_from(n).unwrap_or(i64::MAX));
    J::obj([
        ("frame", J::s(name)),
        ("body", f.body.map_or(J::Null, span_json)),
        ("letters", count(m.letters.len())),
        ("letter_px", J::Int(m.letter_px)),
        ("shaved", count(m.shaved)),
        ("caret_px", J::Int(m.caret_px)),
        ("icon_px", J::Int(m.icon_px)),
        ("icons_read", J::Bool(f.icons_read)),
        ("cells", cells_json(m)),
        ("head", f.head.map_or(J::Null, span_json)),
        ("head_letters", count(h.letters.len())),
        ("head_shaved", count(h.shaved)),
        ("head_caret_px", J::Int(h.caret_px)),
        ("head_icon_px", J::Int(h.icon_px)),
        ("head_cells", cells_json(h)),
    ])
}

/// What `trail-companion` was asked.
#[derive(Clone, Debug, PartialEq)]
pub struct Args {
    pub paths: Vec<String>,
    pub shave: f64,
    pub max: Option<i64>,
    pub list_all: bool,
}

/// # Errors
/// An unknown flag, a malformed number, or no path.
pub fn parse_args(args: &[String]) -> Result<Args, Fail> {
    let mut rest = args.to_vec();
    let shave = match take_value(&mut rest, "--shave")? {
        Some(v) => v
            .parse::<f64>()
            .ok()
            .filter(|s| (0.0..0.5).contains(s))
            .ok_or_else(|| Fail::usage(format!("--shave wants a share in [0, 0.5), got {v}")))?,
        None => SHAVE,
    };
    let max = match take_value(&mut rest, "--max")? {
        Some(v) => Some(
            v.parse::<i64>()
                .map_err(|_| Fail::usage(format!("--max wants an integer, got {v}")))?,
        ),
        None => None,
    };
    let list_all = match take_values(&mut rest, "--list")?.last().map(String::as_str) {
        None | Some("hits") => false,
        Some("all") => true,
        Some(v) => return Err(Fail::usage(format!("--list wants all or hits, got {v}"))),
    };
    if let Some(flag) = rest.iter().find(|a| a.starts_with("--")) {
        return Err(Fail::usage(format!("trail-companion: unknown flag {flag}")));
    }
    if rest.is_empty() {
        return Err(Fail::usage(
            "trail-companion: name a gallery directory or a frame",
        ));
    }
    Ok(Args {
        paths: rest,
        shave,
        max,
        list_all,
    })
}

/// The report over every frame `a` names.
/// # Errors
/// A path that cannot be walked, or a frame whose sidecars cannot be read.
pub fn report(a: &Args) -> Result<J, String> {
    let (mut frames, mut with_body, mut on_letters, mut shaved, mut on_caret, mut on_icons) =
        (0i64, 0i64, 0i64, 0i64, 0i64, 0i64);
    let (mut over, mut icons_unread) = (0i64, 0i64);
    let (mut with_head, mut head_over, mut heads_unread) = (0i64, 0i64, 0i64);
    let mut listed = Vec::new();
    for p in &a.paths {
        let root = PathBuf::from(from_caller(p));
        let base = if root.is_dir() {
            root.clone()
        } else {
            root.parent().map_or_else(PathBuf::new, Path::to_path_buf)
        };
        for png in frames_under(&root)? {
            let Some(f) = read_frame(&png, a.shave)? else {
                continue;
            };
            frames += 1;
            heads_unread += i64::from(!f.head_read);
            if !f.bodied() {
                if a.list_all {
                    listed.push(frame_json(&f, &base));
                }
                continue;
            }
            with_body += 1;
            icons_unread += i64::from(!f.icons_read);
            let (m, h) = (&f.meets, &f.head_meets);
            on_letters += i64::from(!m.letters.is_empty() || !h.letters.is_empty());
            shaved += i64::from(m.shaved > 0 || h.shaved > 0);
            on_caret += i64::from(m.caret_px > 0 || h.caret_px > 0);
            on_icons += i64::from(m.icon_px > 0 || h.icon_px > 0);
            over += i64::from(f.over());
            with_head += i64::from(f.head.is_some());
            head_over += i64::from(h.over());
            if a.list_all || f.over() {
                listed.push(frame_json(&f, &base));
            }
        }
    }
    let mut failures = Vec::new();
    if let Some(max) = a.max
        && over > max
    {
        failures.push(J::s(format!("companion_over_letters {over} > --max {max}")));
    }
    Ok(J::obj([
        ("frames", J::Int(frames)),
        ("with_body", J::Int(with_body)),
        ("companion_over_letters", J::Int(over)),
        ("on_letters", J::Int(on_letters)),
        ("shaved", J::Int(shaved)),
        ("shave", J::Float(round_to(a.shave, 3))),
        ("on_caret", J::Int(on_caret)),
        ("on_icons", J::Int(on_icons)),
        ("icons_unread", J::Int(icons_unread)),
        ("with_head", J::Int(with_head)),
        ("head_over", J::Int(head_over)),
        ("heads_unread", J::Int(heads_unread)),
        ("listed", J::Arr(listed)),
        ("failures", J::Arr(failures)),
    ]))
}

/// The report with one listed frame per line.
#[must_use]
pub fn render(v: &J) -> String {
    let J::Obj(m) = v else {
        return compact(v);
    };
    let items: Vec<String> = m
        .iter()
        .map(|(k, item)| match item {
            J::Arr(a) if !a.is_empty() && k == "listed" => {
                let rows: Vec<String> = a.iter().map(|i| format!("  {}", compact(i))).collect();
                format!(" {}: [\n{}\n ]", J::s(k.as_str()).py(), rows.join(",\n"))
            }
            _ => format!(" {}: {}", J::s(k.as_str()).py(), compact(item)),
        })
        .collect();
    format!("{{\n{}\n}}", items.join(",\n"))
}

/// `trail-companion`: print the report; exit 1 when it is over `--max`.
/// # Errors
/// A usage, file or parse error (exit 2).
pub fn cmd_trail_companion(args: &[String]) -> Res {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return Ok(0);
    }
    let a = parse_args(args)?;
    let out = report(&a).map_err(|e| Fail::usage(format!("trail-companion: {e}")))?;
    println!("{}", render(&out));
    let failed = out
        .get("failures")
        .and_then(J::as_arr)
        .is_some_and(|f| !f.is_empty());
    Ok(i32::from(failed))
}

#[cfg(test)]
#[path = "trail_companion_tests.rs"]
mod tests;
