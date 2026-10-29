// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Prism wake on a landing no frame watched, driven through the app's own frame on the
//! trail-diff rig: a Claude Code relaunch draws its composer while no frame is drawn (an occluded
//! window, a headless capture), and the frame that first shows it is history, not arrival, so the
//! output streak lays no light on its `❯`. A landing a frame watched still sweeps. Each of the
//! host's streak paths but one is driven: one grid's live present, a split's pane beside the
//! focused one, and a single grid's windowless capture. A native page beside the landing pane
//! (`compose_heterogeneous_output_streak`) and a capture of a split are not; both stamp the draw
//! time through the same `note_drawn_at` the split's case breaks in its negative control.

use crate::trail_rig::TrailRig;
use std::time::{Duration, Instant};

/// The font the rig draws, as the dump draws it.
const FIXTURE_FONT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../aterm-render/tests/fixtures/jetbrains-mono.ttf"
);

/// The row the new composer's `❯` lands on (0-based) on the rig's 24-row grid, which is also its
/// rule's 1-based row.
const PROMPT_ROW: u16 = 21;

/// Which of the host's frame paths sees the landing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Path {
    /// The live present of one grid (`presented_frame_at`, the single grid's `streak_grid`).
    Single,
    /// The live present of a split, the landing in the pane beside the focused one: its
    /// snapshot is the window's `unfocused_pane_scratch`, its engine `streak_pane`'s.
    Beside,
    /// A windowless capture (`windowless_frame_at`, a `--no-windows` instance's `image`) on a
    /// window with no present target, at the instant the capture is handed.
    Capture,
}

/// Where the landing's pane stands in the window.
#[derive(Clone, Copy, Debug)]
struct Pane {
    /// Its index in pane order.
    index: usize,
    /// Its first window row.
    row_off: u16,
    /// Its pixel columns `x0..x1` in the window.
    x0: u16,
    x1: u16,
    /// Its grid.
    rows: u16,
    cols: u16,
}

/// A Claude Code composer `cols` wide whose rule is on 1-based row `rule`: the rule, `❯ ` and the
/// caret cell, the bottom rule and the mode row, the caret parked after the `❯ `.
fn composer(rule: u16, cols: u16) -> Vec<u8> {
    let line = "\u{2500}".repeat(usize::from(cols));
    let mode: String = "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)"
        .chars()
        .take(usize::from(cols))
        .collect();
    let mut s = format!("\x1b[{rule};1H{line}");
    s += &format!("\x1b[{};1H\u{276F} \x1b[7m \x1b[27m", rule + 1);
    s += &format!("\x1b[{};1H{line}", rule + 2);
    s += &format!("\x1b[{};1H{mode}", rule + 3);
    s += &format!("\x1b[{};3H", rule + 1);
    s.into_bytes()
}

/// The old Claude's composer, watched; its exit, the shell's relaunch line and the new Claude's
/// composer at `landed`, the last rows of the pane `path` lands in; then frames on `path` at
/// `seen`, `seen + 16 ms` and `seen + 32 ms`, each read for the streak's light on the new
/// composer's prompt row in that pane.
fn landing(path: Path, landed: u64, seen: u64) -> Vec<Vec<(u16, u16, u16)>> {
    let mut rig = TrailRig::new(std::path::Path::new(FIXTURE_FONT)).expect("the rig");
    let t0 = rig.t0();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    rig.configure("trail_sounds = false\nmotion = \"full\"\n", t0)
        .expect("the config admits");
    rig.grid(24, 80, t0).expect("the grid");
    let index = if path == Path::Beside {
        rig.split(t0).expect("the split");
        rig.focus_pane(0, t0)
            .expect("the instance's pane takes the focus");
        1
    } else {
        0
    };
    if path == Path::Capture {
        rig.with_app(t0, |app, wid| {
            app.windows.get_mut(&wid).expect("the window").present = None;
        });
    }
    let session = rig.pane_session(index);
    let pane = rig.with_app(t0, |app, wid| {
        let (rows, cols) = (app.windows[&wid].rows, app.windows[&wid].cols);
        let rect = app
            .active_tree(wid)
            .and_then(|tree| {
                tree.compute_layout(rows, cols)
                    .into_iter()
                    .find(|r| r.session == session)
            })
            .map_or((0, 0, rows, cols), |r| {
                (r.row_off, r.col_off, r.rows, r.cols)
            });
        let (cw, _) = app.win_cell_size(wid);
        let px = |c: u16| u16::try_from(usize::from(c) * cw).expect("a pixel column");
        let grid = {
            let term = app
                .pool
                .get(session)
                .expect("the pane's session")
                .term
                .clone();
            let term = crate::term_lock(&term);
            (term.grid().rows(), term.grid().cols())
        };
        assert_eq!(
            grid,
            (rect.2, rect.3),
            "{path:?}: the pane's grid is its rect"
        );
        Pane {
            index,
            row_off: rect.0,
            x0: px(rect.1),
            x1: px(rect.1 + rect.3),
            rows: rect.2,
            cols: rect.3,
        }
    });
    assert_eq!(
        pane.cols < 80,
        path == Path::Beside,
        "{path:?}: the landing's pane {pane:?}"
    );
    let rule = pane.rows - 3;
    if path != Path::Beside {
        assert_eq!(rule, PROMPT_ROW, "{path:?}: the whole grid's prompt row");
    }
    let frame = |rig: &mut TrailRig, now: Instant| match path {
        Path::Single | Path::Beside => rig.frame(now).expect("a frame").is_some(),
        Path::Capture => rig
            .with_app(now, |app, wid| app.windowless_frame_at(wid, now))
            .expect("a windowless frame")
            .is_some(),
    };
    rig.pty_in(pane.index, b"\x1b[2J\x1b[H Claude Code v2.1.305\r\n", at(1));
    rig.pty_in(pane.index, &composer(6, pane.cols), at(2));
    let mut ms = 16;
    while ms <= 1_000 {
        frame(&mut rig, at(ms));
        ms += 16;
    }
    let relaunch: String = "lab$ claude --dangerously-skip-permissions"
        .chars()
        .take(usize::from(pane.cols))
        .collect();
    rig.pty_in(
        pane.index,
        format!("\x1b[10;1H{relaunch}\r\n").as_bytes(),
        at(1_100),
    );
    rig.pty_in(
        pane.index,
        format!("\x1b[{};1H Claude Code v2.1.311\r\n", rule - 3).as_bytes(),
        at(landed),
    );
    rig.pty_in(pane.index, &composer(rule, pane.cols), at(landed));
    let prompt = pane.row_off + rule;
    (0..3u64)
        .map(|k| {
            let now: Instant = at(seen + 16 * k);
            assert!(frame(&mut rig, now), "{path:?}: the frame at {k} draws");
            rig.input()
                .nova_add
                .iter()
                .filter(|q| q.row == prompt && pane.x0 <= q.x && q.x < pane.x1)
                .map(|q| (q.row, q.x, q.w))
                .collect()
        })
        .collect()
}

/// The landing on `path`, seen 16 ms after it was drawn (the CONTROL: it sweeps its `❯`) and
/// 15 s after (no light on its row on that frame or the two after it).
fn unwatched_takes_no_streak(path: Path) {
    let watched = landing(path, 2_000, 2_016);
    assert!(
        !watched[0].is_empty(),
        "{path:?}: CONTROL: a watched landing sweeps its prompt: {watched:?}"
    );
    let unwatched = landing(path, 2_000, 17_000);
    assert!(
        unwatched.iter().all(Vec::is_empty),
        "{path:?}: an unwatched landing's prompt took light: {unwatched:?}"
    );
}

/// AN UNWATCHED LANDING'S PROMPT TAKES NO STREAK: the new composer drawn while no frame was
/// drawn reads, on the first frame 15 s later and the two after it, with no prism wake light on
/// its row (the green lift on the `❯` cell the captures measured). CONTROL: the same landing seen
/// one frame after it was drawn sweeps its `❯`.
#[test]
fn an_unwatched_landing_s_prompt_takes_no_streak() {
    unwatched_takes_no_streak(Path::Single);
}

/// AN UNWATCHED LANDING IN THE PANE BESIDE TAKES NO STREAK: the same landing in a split's
/// unfocused pane, read through the composed frame's own snapshot of that pane and its pane
/// engine. CONTROL: seen one frame after it was drawn it sweeps.
#[test]
fn an_unwatched_landing_in_the_pane_beside_takes_no_streak() {
    unwatched_takes_no_streak(Path::Beside);
}

/// AN UNWATCHED LANDING TAKES NO STREAK IN A WINDOWLESS CAPTURE: a `--no-windows` instance's
/// capture handed an instant 15 s after the landing was drawn. CONTROL: a capture 16 ms after the
/// draw sweeps.
#[test]
fn an_unwatched_landing_takes_no_streak_in_a_windowless_capture() {
    unwatched_takes_no_streak(Path::Capture);
}
