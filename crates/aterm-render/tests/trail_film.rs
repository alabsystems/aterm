// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// The film `targo --unverified lab trail-film` judges: one scene replayed through the shipping
// effects pipeline and the real renderer ([`Replay`], headless, on the replay's own clock), the
// host's post passes on, every 16 ms frame written as the trail gallery writes a real one:
// `film.<ms>.png`, its clean twin, and the text, cursor and status sidecars, with one
// `film.dims.txt`. `aterm-lab`'s frame readers (`trail-contrast`, `trail-companion`,
// `trail-census`) read a film's frame as they read a gallery's.

/// The resident scenes' pages and lines, the ones `lab trail-gallery`'s line fixture lays in
/// a real aterm's tab: one source, so a scene is one page in a film and in a gallery frame.
#[path = "../../aterm-lab/src/trail_scenes.rs"]
mod trail_scenes;

use crate::trail_letters_read::*;
use aterm_core::terminal::CursorStyle;
use aterm_effects::cursor_fx::TrailColor;
use aterm_render::Frame;
use std::path::Path;
use std::sync::mpsc::sync_channel;
use trail_scenes::{
    BUILD_COMMAND, BUILD_FINISHED, BUILD_LINE_MS, BUILD_LINES, COLOURED_PROMPT_TEXT,
    ENTER_LONG_TEXT, MARK_INPUT, MARK_PROMPT, MARK_RUNS, RESIDENT_SETTLE_MS, RESIDENT_TEXT,
    SHELL_PROMPT, VERDICT_COMMANDS, VERDICT_INTERRUPT_MS, VERDICT_INTERRUPTED, build_line,
    composer_page, composer_row, composer_row_of, exit_mark, fish_row,
};

/// The themes a film is photographed on, by the name `lab trail-film --theme` takes.
const THEMES: [(&str, Look); 6] = [
    ("dark", DARK),
    ("github-light", GITHUB_LIGHT),
    ("default-light", DEFAULT_LIGHT),
    ("solarized-dark", SOLARIZED_DARK),
    ("dim-grey", DIM_GREY),
    ("paper-grey", PAPER_GREY),
];

/// The caret shapes, each with the DECSCUSR that sets it.
const CARETS: [(&str, &[u8]); 3] = [
    ("block", b""),
    ("bar", b"\x1b[6 q"),
    ("underline", b"\x1b[4 q"),
];

/// What a film replays: a gallery scene, or one of the film's own.
#[derive(Clone, Copy)]
pub(crate) enum Take {
    /// One of the replay's gallery scenes.
    Gallery(Scene),
    /// [`Scene::Typing`] with twelve Backspaces after it ([`backspace_events`]).
    Backspace,
    /// A shell prompt on the last row of a full page ([`shell_page`]), output on the row over
    /// it, [`RESIDENT_TEXT`] typed along it and held: where a person types in a shell.
    BottomPrompt,
    /// Claude Code's composer at the foot of its transcript ([`composer_page`]): `❯` between
    /// two grey rules, two grey rows under them, [`RESIDENT_TEXT`] typed into it and held, the program
    /// repainting the row at each key: where a person types to an agent.
    Composer,
    /// [`RESIDENT_TEXT`] typed on a blank page's prompt, then Home and End, each a move of the
    /// whole line's length along its row.
    Jump,
    /// [`ENTER_LONG_TEXT`], a 40-cell command, typed on [`Take::BottomPrompt`]'s page and
    /// entered, its answer scrolling the page.
    EnterLong,
    /// The burst's line on the last row of [`Take::BottomPrompt`]'s page, on a grid this many
    /// columns wide: the line is 69 cells, so 70 and 71 leave one blank cell and two past it.
    Wall(u16),
    /// [`COLOURED_PROMPT_TEXT`] typed on [`Take::BottomPrompt`]'s page as fish paints it: the
    /// line repainted at each key, each word in its syntax ink, the rest of the suggestion grey
    /// past the caret.
    ColouredPrompt,
    /// Five commands on [`Take::BottomPrompt`]'s page, each between shell-integration marks:
    /// `true`, `false`, `sleep 3`, `sleep 3; false`, and a `sleep 9` that Ctrl-C ends (130).
    Verdict,
    /// A build entered on [`Take::BottomPrompt`]'s page: forty lines of output 100 ms apart,
    /// then a green end (exit 0) and the next prompt.
    LongBuild,
}

/// The scenes, by the name `lab trail-film --scene` takes.
const SCENES: [(&str, Take); 25] = [
    ("typing", Take::Gallery(Scene::Typing)),
    ("burst", Take::Gallery(Scene::Burst)),
    ("context", Take::Gallery(Scene::Context)),
    ("enter", Take::Gallery(Scene::EnterBottom)),
    ("backspace", Take::Backspace),
    ("bottom-prompt", Take::BottomPrompt),
    ("composer", Take::Composer),
    ("jump", Take::Jump),
    ("enter-long", Take::EnterLong),
    ("wall-69", Take::Wall(69)),
    ("wall-70", Take::Wall(70)),
    ("wall-71", Take::Wall(71)),
    ("wall-72", Take::Wall(72)),
    ("wall-73", Take::Wall(73)),
    ("coloured-prompt", Take::ColouredPrompt),
    ("verdict", Take::Verdict),
    ("long-build", Take::LongBuild),
    ("steam", Take::Gallery(Scene::Steam)),
    ("cjk", Take::Gallery(Scene::Cjk)),
    ("wrapped", Take::Gallery(Scene::Wrapped)),
    ("fold-rest", Take::Gallery(Scene::FoldRest)),
    ("last-row", Take::Gallery(Scene::LastRow)),
    ("move-up", Take::Gallery(Scene::MoveUp)),
    ("split", Take::Gallery(Scene::SplitBurst)),
    ("focus-wrap", Take::Gallery(Scene::FocusWrap)),
];

/// When a film of the film's own types its first key, ms.
const SETTLED: u64 = RESIDENT_SETTLE_MS;

/// A full page of a shell's session on `rows` by `cols` (`trail_scenes::shell_page`), then the
/// prompt on the last row between its shell-integration marks, as aterm's own shell
/// integration writes one.
fn shell_page(rows: u16, cols: u16) -> Vec<u8> {
    (trail_scenes::shell_page(rows, cols) + &prompt()).into_bytes()
}

/// A shell's prompt between its marks (OSC 133 A and B).
fn prompt() -> String {
    format!("{MARK_PROMPT}{SHELL_PROMPT}{MARK_INPUT}")
}

/// An Enter on a shell's line: the newline, and the mark that the command runs (OSC 133 C).
fn enter(at: u64) -> (u64, Ev) {
    (at, Ev::Key('\r', format!("\r\n{MARK_RUNS}").into_bytes()))
}

/// A command's end at `at`: its `output`, its exit mark (OSC 133 D) and the next prompt.
fn done(at: u64, output: &str, code: u8) -> (u64, Ev) {
    let bytes = format!("{output}{}{}", exit_mark(code), prompt());
    (at, Ev::Out(bytes.into_bytes()))
}

impl Take {
    /// The film's pane, rows and columns.
    pub(crate) fn grid(self) -> (u16, u16) {
        match self {
            Take::Gallery(scene) => (scene.rows(), scene.cols()),
            Take::Wall(cols) => (ROWS, cols),
            _ => (ROWS, COLS),
        }
    }

    /// The film's events on `look`, and how long it runs past the last, ms.
    pub(crate) fn events(self, look: Look) -> (Vec<(u64, Ev)>, u64) {
        let (rows, cols) = self.grid();
        let page = |bytes: Vec<u8>| vec![(200, Ev::Out(bytes))];
        match self {
            Take::Gallery(scene) => scene.events_on(look),
            Take::Backspace => backspace_events(look),
            Take::BottomPrompt => {
                let mut ev = page(shell_page(rows, cols));
                ev.extend(typed(RESIDENT_TEXT, SETTLED, 60));
                (ev, 1_500)
            }
            Take::Composer => {
                let row = composer_row_of(rows);
                let mut ev = page((composer_page(rows, cols) + &composer_row(row, "")).into());
                for (i, (at, key)) in typed(RESIDENT_TEXT, SETTLED, 60).into_iter().enumerate() {
                    let Ev::Key(c, _) = key else { continue };
                    let repaint = composer_row(row, &RESIDENT_TEXT[..=i]);
                    ev.push((at, Ev::Key(c, repaint.into_bytes())));
                }
                (ev, 1_500)
            }
            Take::Jump => {
                let mut ev = page(b"\x1b[2J\x1b[5H$ ".to_vec());
                ev.extend(typed(RESIDENT_TEXT, SETTLED, 50));
                let typed_by = SETTLED + 50 * RESIDENT_TEXT.len() as u64;
                ev.push((typed_by + 600, Ev::Key(HOME, b"\x1b[3G".to_vec())));
                let end = format!("\x1b[{}G", RESIDENT_TEXT.len() + 3);
                ev.push((typed_by + 2_100, Ev::Key(END, end.into_bytes())));
                (ev, 2_000)
            }
            Take::EnterLong => {
                let mut ev = page(shell_page(rows, cols));
                ev.extend(typed(ENTER_LONG_TEXT, SETTLED, 50));
                let at = SETTLED + 50 * ENTER_LONG_TEXT.len() as u64 + 200;
                ev.push(enter(at));
                let answer = format!("{}\r\n", &ENTER_LONG_TEXT["echo ".len()..]);
                ev.push(done(at + 6, &answer, 0));
                (ev, 1_500)
            }
            Take::Wall(_) => {
                let mut ev = page(shell_page(rows, cols));
                ev.extend(typed(BURST_TEXT, SETTLED, 50));
                (ev, 4_600)
            }
            Take::ColouredPrompt => {
                let text = COLOURED_PROMPT_TEXT;
                let mut ev = page(shell_page(rows, cols));
                ev.push((SETTLED - 500, Ev::Out(fish_row(rows, "").into_bytes())));
                for (i, (at, key)) in typed(text, SETTLED, 60).into_iter().enumerate() {
                    let Ev::Key(c, _) = key else { continue };
                    ev.push((at, Ev::Key(c, fish_row(rows, &text[..=i]).into_bytes())));
                }
                (ev, 1_500)
            }
            Take::Verdict => {
                let mut ev = page(shell_page(rows, cols));
                let mut at = SETTLED;
                for (_, command, runs, code) in VERDICT_COMMANDS {
                    // A command that runs no time still ends a moment after its Enter.
                    let runs = runs.max(6);
                    ev.extend(typed(command, at, 60));
                    let entered = at + 60 * command.len() as u64 + 150;
                    ev.push(enter(entered));
                    ev.push(done(entered + runs, "", code));
                    at = entered + runs + 1_500;
                }
                ev.extend(typed(VERDICT_INTERRUPTED, at, 60));
                let entered = at + 60 * VERDICT_INTERRUPTED.len() as u64 + 150;
                ev.push(enter(entered));
                ev.push(done(entered + VERDICT_INTERRUPT_MS, "^C\r\n", 130));
                (ev, 2_000)
            }
            Take::LongBuild => {
                let command = BUILD_COMMAND;
                let mut ev = page(shell_page(rows, cols));
                ev.extend(typed(command, SETTLED, 60));
                let entered = SETTLED + 60 * command.len() as u64 + 150;
                ev.push(enter(entered));
                for n in 0..BUILD_LINES {
                    let at = entered + BUILD_LINE_MS * (n + 1);
                    ev.push((at, Ev::Out(build_line(n).into_bytes())));
                }
                let ended = entered + BUILD_LINE_MS * (BUILD_LINES + 1);
                ev.push(done(ended, BUILD_FINISHED, 0));
                (ev, 2_500)
            }
        }
    }
}

/// [`Scene::Typing`], then twelve Backspaces 50 ms apart from 60 ms after its last key, each
/// the console's delete with its erase echoed, and the fade: held Backspace on a plain page.
fn backspace_events(look: Look) -> (Vec<(u64, Ev)>, u64) {
    let (mut ev, _) = Scene::Typing.events_on(look);
    let start = 1_000 + 60 * TYPING_TEXT.len() as u64 + 60;
    ev.extend((0..12u64).map(|k| (start + 50 * k, Ev::Key('\u{7f}', b"\x08\x1b[K".to_vec()))));
    (ev, 1_400)
}

/// `name` among `table`'s names, or a panic that lists them.
fn named<T: Copy>(what: &str, name: &str, table: &[(&str, T)]) -> T {
    table
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, v)| *v)
        .unwrap_or_else(|| {
            let names: Vec<&str> = table.iter().map(|(n, _)| *n).collect();
            panic!("no {what} named {name:?}: {}", names.join(", "))
        })
}

/// The caret's style as `aterm ctl cursor` names it.
fn style_word(style: CursorStyle) -> &'static str {
    match style {
        CursorStyle::BlinkingBlock => "blinking_block",
        CursorStyle::SteadyBlock => "steady_block",
        CursorStyle::BlinkingUnderline => "blinking_underline",
        CursorStyle::SteadyUnderline => "steady_underline",
        CursorStyle::BlinkingBar => "blinking_bar",
        CursorStyle::SteadyBar => "steady_bar",
        _ => "unknown",
    }
}

fn boxed(b: Option<(i32, i32, i32, i32)>) -> String {
    b.map_or_else(
        || "none".to_string(),
        |(x0, x1, y0, y1)| format!("{x0},{x1},{y0},{y1}"),
    )
}

/// One kept frame's files under `dir`; `clean` is the last twin written and its path, which a
/// frame with the same twin links instead of encoding it again.
fn write_frame(
    dir: &Path,
    style: &str,
    k: &Kept,
    clean: &mut Option<(Vec<u32>, std::path::PathBuf)>,
) {
    let put = |name: String, bytes: &[u8]| {
        let path = dir.join(name);
        std::fs::write(&path, bytes)
            .unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
        path
    };
    let png = |px: &[u32]| {
        Frame {
            width: k.w,
            height: px.len() / k.w,
            pixels: px.to_vec(),
        }
        .to_png()
    };
    let stem = format!("film.{:06}", k.ms);
    put(format!("{stem}.png"), &png(&k.px));
    let twin = dir.join(format!("{stem}.clean.png"));
    match clean {
        Some((px, path)) if *px == k.clean => {
            std::fs::hard_link(&*path, &twin)
                .unwrap_or_else(|e| panic!("cannot link {}: {e}", twin.display()));
        }
        _ => {
            put(format!("{stem}.clean.png"), &png(&k.clean));
            *clean = Some((k.clean.clone(), twin));
        }
    }
    let g = &k.glass;
    let text = g.text.join("\n") + "\n";
    put(format!("{stem}.text.txt"), text.as_bytes());
    if text.trim().is_empty() {
        // A blank screen read: the gallery keeps a reply's `OK` header in this twin, by which
        // `trail-companion` tells an empty page from a read that failed.
        put(
            format!("{stem}.text.txt.stderr"),
            format!("OK {}\n", text.len()).as_bytes(),
        );
    }
    put(
        format!("{stem}.cursor.txt"),
        format!(
            "OK {} {} {} {}\n",
            k.caret.0,
            k.caret.1,
            u8::from(g.caret_visible),
            style_word(g.caret_style)
        )
        .as_bytes(),
    );
    let icons = if g.word_icons.is_empty() {
        "none".to_string()
    } else {
        let each: Vec<String> = g.word_icons.iter().map(|&b| boxed(Some(b))).collect();
        each.join(";")
    };
    let [quads, halos, stars, meteors] = g.v2.unwrap_or_default();
    put(
        format!("{stem}.status.txt"),
        format!(
            "trail style={style:?} pet_action={} pet_body={} head_body={} pet_pose={} pet_alpha={} pet_seat={} \
             v2={} v2_quads={quads} v2_halos={halos} v2_stars={stars} v2_meteors={meteors} \
             word_icons={icons}\n",
            g.pet_action,
            boxed(g.pet_body),
            boxed(g.head_body),
            g.pet_pose,
            g.pet_alpha,
            g.pet_seat,
            g.v2.is_some(),
        )
        .as_bytes(),
    );
}

/// A replay as a film runs one: the host's post passes on, and the effects given what the
/// window's host gives them and [`Replay`]'s bare pipeline does not.
///
/// - The rows over and under the caret's (`EffectsPipeline::set_probe_flanks`). Without them
///   the ribbon does not know a row has letters under it and no star is born by a row nobody
///   read: a film of the composer drew a rail under its row and no star, where the real app
///   (`lab trail-gallery`) draws no rail and a few stars
///   ([`a_film_holds_the_composer_s_row_as_the_window_does`]).
/// - Sparkle words, which the app has on by default: the icon over a word such as "fox".
///
/// Still not the window's: its 12 px of padding round the grid, its presents (a frame is
/// every 16 ms of the replay's clock), and the caret a program has not set (the terminal's
/// blinking block, drawn steady).
pub(crate) fn film_replay(style: &str, look: Look, rows: u16, cols: u16) -> Replay {
    let mut r = Replay::gridded(style, look, TrailColor::Automatic, rows, cols);
    r.post = true;
    r.fx.set_probe_flanks(true);
    r.fx.set_sparkle_enabled(true);
    r
}

/// One film: `take` on `look` with the caret `caret` sets, every frame written under `dir`.
fn film_one(dir: &Path, style: &str, look: Look, caret: &[u8], take: Take) -> usize {
    let (mut ev, tail) = take.events(look);
    if !caret.is_empty() {
        ev.insert(0, (0, Ev::Out(caret.to_vec())));
    }
    let (rows, cols) = take.grid();
    let end = ev.last().map_or(0, |e| e.0) + tail;
    let frames: Vec<u64> = (1..=end / 16).map(|k| k * 16).collect();
    let (to, kept) = sync_channel::<Kept>(2);
    let mut written = 0usize;
    std::thread::scope(|s| {
        // Owned here, the reader goes with a failed write, and so the replay with it.
        let kept = kept;
        s.spawn(|| {
            let mut r = film_replay(style, look, rows, cols);
            r.keep.clone_from(&frames);
            r.keep_to = Some(to);
            let _ = r.run(&ev, tail);
        });
        let mut clean = None;
        for k in kept.iter() {
            if written == 0 {
                let dims = format!(
                    "OK {rows} {cols} {} {} cell_w={} cell_h={} pad={} pad_top={} head=0\n",
                    k.w,
                    k.px.len() / k.w,
                    k.cell.0,
                    k.cell.1,
                    k.left,
                    k.top
                );
                std::fs::write(dir.join("film.dims.txt"), dims).expect("the film's dims");
            }
            write_frame(dir, style, &k, &mut clean);
            written += 1;
        }
    });
    assert_eq!(written, frames.len(), "the replay kept every frame");
    written
}

#[test]
#[ignore = "writes the films `targo --unverified lab trail-film` judges under TRAIL_FILM_OUT, one folder \
            <theme>_<caret>_<scene> each: TRAIL_FILM_STYLE (a trail style) on every one of TRAIL_FILM_THEMES, \
            TRAIL_FILM_CARETS and TRAIL_FILM_SCENES (comma lists of the names in this file's tables)"]
fn film() {
    let var = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("{k} is not set"));
    let root = std::path::PathBuf::from(var("TRAIL_FILM_OUT"));
    let style = var("TRAIL_FILM_STYLE");
    let mut jobs = Vec::new();
    for theme in var("TRAIL_FILM_THEMES").split(',') {
        for caret in var("TRAIL_FILM_CARETS").split(',') {
            for scene in var("TRAIL_FILM_SCENES").split(',') {
                jobs.push((
                    root.join(format!("{theme}_{caret}_{scene}")),
                    named("theme", theme, &THEMES),
                    named("caret", caret, &CARETS),
                    named("scene", scene, &SCENES),
                ));
            }
        }
    }
    let jobs = std::sync::Mutex::new(jobs);
    let workers = aterm_cores::available_parallelism().map_or(2, |n| n.get().min(6));
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                loop {
                    let Some((dir, look, caret, scene)) = jobs.lock().expect("the jobs").pop()
                    else {
                        return;
                    };
                    std::fs::create_dir_all(&dir).expect("the film's folder");
                    let written = film_one(&dir, &style, look, caret, scene);
                    println!("FILM frames={written} every_ms=16 dir={}", dir.display());
                }
            });
        }
    });
}

/// The pixels a kept frame's effects changed in the rail's place under the caret's row (the
/// [`RAIL_BAND`] px rows under it, from the grid's left to the caret), and its stars.
fn rail_and_stars(k: &Kept) -> (usize, u32) {
    let (cw, ch) = k.cell;
    let top = k.top + (k.caret.0 + 1) * ch;
    let h = k.px.len() / k.w;
    let lit = RAIL_BAND
        .map(|dy| top + dy)
        .filter(|&y| y < h)
        .flat_map(|y| (k.left..k.left + k.caret.1 * cw).map(move |x| y * k.w + x))
        .filter(|&i| k.px[i] != k.clean[i])
        .count();
    (lit, k.glass.v2.map_or(0, |v| v[2]))
}

/// The composer's row at its last key, as a film replays it and as a bare [`Replay`] does.
/// The real app, read from `lab trail-gallery`'s frames, draws no rail under that row (the
/// rule under it holds the ribbon in the row) and has stars alive; a replay that hands the
/// effects no neighbour rows draws the whole rail and no star, which is what the film read
/// before it asked for them.
#[test]
fn a_film_holds_the_composer_s_row_as_the_window_does() {
    let take = Take::Composer;
    let (rows, cols) = take.grid();
    let (ev, _) = take.events(DARK);
    let last = ev.last().expect("the composer's keys").0;
    let at = (last / 16 + 2) * 16;
    let read = |flanks: bool| {
        let mut r = film_replay("underline rainbow + cat", DARK, rows, cols);
        r.fx.set_probe_flanks(flanks);
        r.keep = vec![at];
        let _ = r.run(&ev, at - last);
        let k = r.kept.pop().expect("the frame at the last key");
        assert_eq!(
            k.glass.text[k.caret.0],
            format!("\u{276f}\u{a0}{RESIDENT_TEXT}")
        );
        (rail_and_stars(&k), k.glass.word_icons.len(), k.cell.0)
    };
    let ((rail, stars), icons, _) = read(true);
    assert_eq!(rail, 0, "the rule under the row holds the ribbon inside it");
    assert!(stars > 0, "stars are born by rows the effects read");
    assert_eq!(icons, 1, "sparkle words draw the fox's icon");
    // The negative control: no neighbour rows, and the rail is laid under the line's 47 cells
    // (nine tenths or more of their band's pixels, on the cell the face lays: 2632 px measured
    // on macOS's 14 px wide cell) with no star.
    let ((rail, stars), _, cw) = read(false);
    let band = 47 * cw * RAIL_BAND.len();
    assert!(rail * 10 >= band * 9, "{rail} of {band} px");
    assert_eq!(stars, 0);
}
