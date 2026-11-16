// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// The film `targo --unverified lab trail-film` judges: one scene replayed through the shipping
// effects pipeline and the real renderer ([`Replay`], headless, on the replay's own clock), the
// host's post passes on, every 16 ms frame written as the trail gallery writes a real one:
// `film.<ms>.png`, its clean twin, and the text, cursor and status sidecars, with one
// `film.dims.txt`. `aterm-lab`'s frame readers (`trail-contrast`, `trail-companion`,
// `trail-census`) read a film's frame as they read a gallery's. A film is shot in one of two
// lanes ([`Lane`]): the replay's own (the web host's frame, no light) or a Retina window's (its
// face and padding, and its light latched as the window latches it), where a light page draws
// the prism glass.

/// The resident scenes' pages and lines, the ones `lab trail-gallery`'s line fixture lays in
/// a real aterm's tab: one source, so a scene is one page in a film and in a gallery frame.
#[path = "../../aterm-lab/src/trail_scenes.rs"]
mod trail_scenes;

use crate::trail_letters_read::*;
use aterm_core::terminal::CursorStyle;
use aterm_effects::cursor_fx::TrailColor;
use aterm_effects::cursor_glow::Light;
use aterm_render::{Frame, Renderer, Theme};
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

/// The caret shapes, each with the DECSCUSR that sets it; `block-pinned`, `bar-pinned` and
/// `underline-pinned` are the shape with its colour pinned by the program (OSC 12), which the
/// effects wear and move only where the light round it would read it under its own reading.
const CARETS: [(&str, &[u8]); 6] = [
    ("block", b""),
    ("bar", b"\x1b[6 q"),
    ("underline", b"\x1b[4 q"),
    ("block-pinned", b"\x1b]12;#cf222e\x07"),
    ("bar-pinned", b"\x1b[6 q\x1b]12;#cf222e\x07"),
    ("underline-pinned", b"\x1b[4 q\x1b]12;#cf222e\x07"),
];

/// Where a film is shot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lane {
    /// The replay's own: [`Replay::gridded`]'s face at [`GALLERY_PX`] with no padding, through
    /// the web host's frame, which latches no light (`EffectsPipeline::apply`): every page,
    /// light ones included, draws main's paper arm.
    Paper,
    /// A Retina Mac's window (`TRAIL_FILM_GLASS=1`, `lab trail-film --glass`): the window's
    /// face at [`RETINA_PX`] in its padding ([`RETINA_PAD`], [`RETINA_PAD_TOP`]) and frame
    /// ([`window_frame`]), its effects laid from the grid's corner as the window lays them
    /// ([`window_replay`]), and the frame grounded as the window grounds it (`EffectsPipeline::ground_as_window`): its
    /// light latch on the live page at the live level, on an opaque page with no wallpaper
    /// whose thin ink is the face's own. A light page latches at once and, where its face
    /// keeps its letters under the glass (`PageFacts::glass`), draws the glass; a dark page
    /// never latches, and draws what [`Lane::Paper`] draws in this face.
    Glass,
    /// [`Lane::Glass`]'s window with its latch dropped (`TRAIL_FILM_GLASS=unlatched`, `lab trail-film --glass
    /// --unlatched`): its face and padding, the web host's frame, main's paper band on a light page. The glass
    /// lane's control: what of a glass film's reading is the glass's, and what the Retina face's.
    Unlatched,
}

/// A Retina Mac's display scale.
const RETINA_SCALE: f32 = 2.0;
/// A Retina window's face, device px: the window's face on macOS
/// ([`aterm_render::WINDOW_FONT_PX_MACOS`], `aterm-gui`'s `FONT_PX`) at [`RETINA_SCALE`], the
/// size `Renderer::from_system` lays SF Mono's 15 x 28 px cell at.
pub(crate) const RETINA_PX: f32 = aterm_render::WINDOW_FONT_PX_MACOS * RETINA_SCALE;
/// A Retina window's padding round the grid, device px: the window's
/// ([`aterm_render::WINDOW_PAD_LOGICAL_PX`]) at [`RETINA_SCALE`], rounded as `aterm-gui`
/// rounds it ...
pub(crate) const RETINA_PAD: usize = device_px(aterm_render::WINDOW_PAD_LOGICAL_PX);
/// ... and over the grid ([`aterm_render::WINDOW_PAD_TOP_LOGICAL_PX`]).
pub(crate) const RETINA_PAD_TOP: usize = device_px(aterm_render::WINDOW_PAD_TOP_LOGICAL_PX);

/// `logical` px at [`RETINA_SCALE`], rounded half up (`aterm-gui`'s `logical_to_device_px`).
const fn device_px(logical: f32) -> usize {
    (logical * RETINA_SCALE + 0.5) as usize
}

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
            "trail style={style:?} pet_action={} pet_body={} head_body={} head_alpha={} head_pose={} pet_pose={} \
             pet_alpha={} pet_seat={} v2={} v2_quads={quads} v2_halos={halos} v2_stars={stars} \
             v2_meteors={meteors} word_icons={icons} light={} live={} sheer={} thin_ink={} arms={}\n",
            g.pet_action,
            boxed(g.pet_body),
            boxed(g.head_body),
            g.head_alpha,
            g.head_pose,
            g.pet_pose,
            g.pet_alpha,
            g.pet_seat,
            g.v2.is_some(),
            g.light.bits(),
            live_word(g.light),
            u8::from(g.light.sheer()),
            g.thin_ink
                .map_or_else(|| "none".to_string(), |t| format!("{t:.4}")),
            g.arms,
        )
        .as_bytes(),
    );
}

/// The highest level a frame's light carries, as `trail light` spells a level (`Live::word`),
/// sheer or not; `-` where no light latched.
fn live_word(light: Light) -> &'static str {
    use aterm_effects::cursor_glow::Live;
    let at = |live: Live| Light::at(live).bits();
    let level = [Live::Vanish, Live::Marks, Live::Lip, Live::Glass, Live::Off]
        .into_iter()
        .find(|&live| light.bits() & at(live) == at(live));
    match level {
        Some(live) if light.on() => live.word(),
        _ => "-",
    }
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
/// In [`Lane::Glass`], also the window's face, padding, effects origin and light. Still not the window's:
/// [`Lane::Paper`]'s padding and light, its presents (a frame is every 16 ms of the replay's
/// clock), and the caret a program has not set (the terminal's blinking block, drawn steady).
pub(crate) fn film_replay(style: &str, look: Look, rows: u16, cols: u16, lane: Lane) -> Replay {
    let mut r = match lane {
        Lane::Paper => Replay::gridded(style, look, TrailColor::Automatic, rows, cols),
        Lane::Glass | Lane::Unlatched => window_replay(
            RETINA_PX,
            style,
            look,
            (rows, cols),
            lane == Lane::Glass,
            true,
        ),
    };
    finish_film_replay(&mut r);
    r
}

/// A Retina window's replay with its face at `px` device px, in the window's padding: its
/// renderer lays the grid [`RETINA_PAD`] in from the left and [`RETINA_PAD_TOP`] down, and its
/// effects lay their window-absolute light from the same origin (`registered`: the window's
/// chrome, `EffectsPipeline::set_chrome` and `set_chrome_pad_top`, as `aterm-gui`'s
/// `effects_origin_win` hands its frame). `grounded`: its light latched as the window latches it.
/// A replay not `registered` lays its effects from the frame's corner while its letters sit in
/// the padding: the films' first glass lane, which the registration test's control keeps.
fn window_replay(
    px: f32,
    style: &str,
    look: Look,
    grid: (u16, u16),
    grounded: bool,
    registered: bool,
) -> Replay {
    let mut rend = Renderer::from_system(px, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature");
    rend.set_pad(RETINA_PAD);
    rend.set_pad_top(RETINA_PAD_TOP);
    let mut r = Replay::gridded_on(rend, style, look, TrailColor::Automatic, grid);
    if registered {
        let pad = |v: usize| u16::try_from(v).expect("a window's pad");
        r.fx.set_chrome(pad(RETINA_PAD), 0);
        r.fx.set_chrome_pad_top(Some(pad(RETINA_PAD_TOP)));
    }
    if grounded {
        r.fx.ground_as_window(Some(aterm_effects::host::PageFacts::OPAQUE));
    }
    r
}

/// What every film's replay gets past its lane's: the host's post passes, the caret's flanking
/// rows and sparkle words.
fn finish_film_replay(r: &mut Replay) {
    r.post = true;
    r.fx.set_probe_flanks(true);
    r.fx.set_sparkle_enabled(true);
}

/// `k` and its clean twin cut to the window's frame: the grid under its pad over the grid
/// (`k.top`), and its pad round the grid (`k.left`) under it, as `aterm-gui` sizes a window
/// (`pad_bottom` is its `pad`). The renderer's frame lays two pads and a head round the grid
/// whatever its pad over it (`Renderer::frame_size`), so a window whose pad over the grid is the
/// less has page under its grid that the window does not: rows nothing draws, and no reader is
/// to read. The paper lane's frame is its grid alone, and is left as it is.
fn window_frame(k: &mut Kept, rows: u16) {
    let h = k.top + usize::from(rows) * k.cell.1 + k.left;
    for px in [&mut k.px, &mut k.clean] {
        assert!(px.len() >= h * k.w, "a frame shorter than its window");
        px.truncate(h * k.w);
    }
}

/// One film: `take` on `look` with the caret `caret` sets, shot in `lane`, every frame written
/// under `dir`.
fn film_one(dir: &Path, style: &str, look: Look, caret: &[u8], take: Take, lane: Lane) -> usize {
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
            let mut r = film_replay(style, look, rows, cols, lane);
            r.keep.clone_from(&frames);
            r.keep_to = Some(to);
            let _ = r.run(&ev, tail);
        });
        let mut clean = None;
        for mut k in kept.iter() {
            window_frame(&mut k, rows);
            if written == 0 {
                // A window's lane says whether its page is one the glass is solved for: a fact
                // of the palette (`prism_glass::headroom`), apart from what the frames' light
                // latched, by which `lab trail-film --glass` asks the glass of the film.
                let page = match lane {
                    Lane::Paper => String::new(),
                    Lane::Glass | Lane::Unlatched => format!(
                        " glass_page={}",
                        u8::from(aterm_effects::rainbow_kitty::prism_glass::headroom(
                            look.fg, look.bg
                        ))
                    ),
                };
                let dims = format!(
                    "OK {rows} {cols} {} {} cell_w={} cell_h={} pad={} pad_top={} head=0{page}\n",
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
            TRAIL_FILM_CARETS and TRAIL_FILM_SCENES (comma lists of the names in this file's tables), in a \
            Retina window's lane where TRAIL_FILM_GLASS is 1 (unlatched: that lane with its latch dropped)"]
fn film() {
    let var = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("{k} is not set"));
    let root = std::path::PathBuf::from(var("TRAIL_FILM_OUT"));
    let style = var("TRAIL_FILM_STYLE");
    let lane = match std::env::var("TRAIL_FILM_GLASS").as_deref() {
        Err(std::env::VarError::NotPresent) | Ok("0") => Lane::Paper,
        Ok("1") => Lane::Glass,
        Ok("unlatched") => Lane::Unlatched,
        other => panic!("TRAIL_FILM_GLASS is 1, unlatched or 0, not {other:?}"),
    };
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
                    let written = film_one(&dir, &style, look, caret, scene, lane);
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
        let mut r = film_replay("underline rainbow + cat", DARK, rows, cols, Lane::Paper);
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

/// `take` on `look`, replayed by `replay` (given the take's grid): the frames at `at`.
fn take_frames(
    take: Take,
    look: Look,
    replay: impl FnOnce((u16, u16)) -> Replay,
    at: &[u64],
) -> Vec<Kept> {
    let (ev, _) = take.events(look);
    let last = ev.last().expect("the scene's keys").0;
    let mut r = replay(take.grid());
    r.keep = at.to_vec();
    let end = *at.iter().max().expect("a frame");
    let _ = r.run(&ev, end.saturating_sub(last) + 16);
    std::mem::take(&mut r.kept)
}

/// The typing scene on `look` shot in `lane`: the frames at `at`.
fn typing_frames(look: Look, lane: Lane, at: &[u64]) -> Vec<Kept> {
    take_frames(
        Take::Gallery(Scene::Typing),
        look,
        |(rows, cols)| film_replay("rainbow", look, rows, cols, lane),
        at,
    )
}

/// The typing scene on `look` in a Retina window's lane built by hand ([`window_replay`]):
/// its face at `px`, its light latched where `grounded`, its effects laid from the grid's
/// corner where `registered`.
fn window_typing(look: Look, px: f32, grounded: bool, registered: bool, at: &[u64]) -> Vec<Kept> {
    take_frames(
        Take::Gallery(Scene::Typing),
        look,
        |grid| {
            let mut r = window_replay(px, "rainbow", look, grid, grounded, registered);
            finish_film_replay(&mut r);
            r
        },
        at,
    )
}

/// Whether `k`'s arms name `arm` (`glass`, `yield`, ...).
fn drew(k: &Kept, arm: &str) -> bool {
    k.glass
        .arms
        .split(',')
        .any(|a| a.split(':').next() == Some(arm))
}

/// The glass's floor (`docs/design/PRISM-GLASS.md` §0: a bed's relative luminance of 0.3505 or
/// more) and the chroma, of 255 (greatest channel less least), its stops sit at or over: a deep
/// pixel of the glass behind a word.
const GLASS_FLOOR: f32 = 0.3505;
const GLASS_CHROMA: i32 = 120;
/// The share of the typed row's page pixels a frame lays deep glass on ([`glass_behind`]):
/// measured 0.62 on GitHub Light's typing scene with the line typed (2528 ms) in [`Lane::Glass`]
/// (a real Retina window's frame with its line typed read 0.63), 0.22 where the same window's
/// light is not latched.
const GLASS_SHARE: f64 = 0.5;

/// How much of the page behind the typed word (the caret's row, from the prompt's end to the
/// caret, its whole height, every pixel its clean twin leaves the page's) `k` lays deep glass
/// on ([`GLASS_FLOOR`], [`GLASS_CHROMA`]), where its light drew the glass; an error naming its
/// light and arms where it did not.
fn glass_behind(k: &Kept, page: u32) -> Result<f64, String> {
    if !k.glass.light.glass() || !drew(k, "glass") {
        return Err(format!(
            "{} ms: no glass drawn (light={} arms={})",
            k.ms,
            k.glass.light.bits(),
            k.glass.arms
        ));
    }
    let (cw, ch) = k.cell;
    let row = k.caret.0;
    let (mut under, mut deep) = (0usize, 0usize);
    for y in k.top + row * ch..k.top + (row + 1) * ch {
        for x in k.left + 2 * cw..k.left + k.caret.1 * cw {
            let i = y * k.w + x;
            if k.clean[i] & 0x00FF_FFFF != page {
                continue;
            }
            under += 1;
            let f = k.px[i] & 0x00FF_FFFF;
            let c = [16u32, 8, 0].map(|sh| ((f >> sh) & 0xff) as i32);
            let chroma = c.iter().max().unwrap_or(&0) - c.iter().min().unwrap_or(&0);
            if chroma >= GLASS_CHROMA && aterm_render::linear_luminance(f) >= GLASS_FLOOR {
                deep += 1;
            }
        }
    }
    Ok(deep as f64 / under.max(1) as f64)
}

/// The typing scene's frame with its whole line typed: its last key at 2380 ms, the hand on.
const LINE_TYPED_MS: u64 = 2_528;

/// A Retina window's light page draws the glass behind the typed word: GitHub Light's typing
/// scene in [`Lane::Glass`] latches its light on the first frame at the live level (`ON|GLASS`,
/// bits 3, on a face whose thin ink is SF Mono's 24 px none), its arms name the glass, and over
/// half the page behind the line lies under deep glass. Negative control: the same window with
/// its latch dropped ([`Lane::Unlatched`]) reads light 0, no glass arm, and lays deep colour on
/// under a quarter of that page (main's paper band), so [`glass_behind`] refuses it on both
/// counts. The window's cell is SF Mono's 15 x 28 px on macOS, the one platform whose
/// [`Renderer::from_system`] loads that face; elsewhere the lane lays the same window round
/// the face that platform loads.
#[test]
fn a_retina_window_s_light_page_draws_the_glass_behind_the_typed_word() {
    use aterm_effects::cursor_glow::Live;
    let page = GITHUB_LIGHT.bg;
    let at = [16, LINE_TYPED_MS];
    let lit = typing_frames(GITHUB_LIGHT, Lane::Glass, &at);
    assert_eq!(
        lit[0].glass.light,
        Light::at(Live::Glass),
        "latched on frame 1"
    );
    let k = &lit[1];
    assert_eq!((k.left, k.top), (RETINA_PAD, RETINA_PAD_TOP));
    if cfg!(target_os = "macos") {
        assert_eq!(k.cell, (15, 28), "SF Mono's cell at {RETINA_PX} px");
    }
    assert!(
        k.glass.thin_ink.is_some_and(|t| t <= 0.007),
        "{:?}",
        k.glass.thin_ink
    );
    let share = glass_behind(k, page).expect("the glass drew");
    assert!(
        share >= GLASS_SHARE,
        "{share:.3} of the page behind the word"
    );
    let paper = typing_frames(GITHUB_LIGHT, Lane::Unlatched, &at);
    let k = &paper[1];
    assert_eq!((k.glass.light, k.glass.arms.as_str()), (Light::OFF, "-"));
    let err = glass_behind(k, page).expect_err("the control draws no glass");
    assert!(err.contains("light=0 arms=-"), "{err}");
    // The pixels alone tell it too: main's paper band is pale where the glass is deep.
    let share = glass_behind(
        &Kept {
            glass: Glass {
                light: Light::at(Live::Glass),
                arms: "glass:glow_under".to_string(),
                ..k.glass.clone()
            },
            px: k.px.clone(),
            clean: k.clean.clone(),
            ..*k
        },
        page,
    )
    .expect("read as if glass");
    assert!(share < GLASS_SHARE, "{share:.3}");
}

/// The paper lane is the replay's own: its frames latch no light on a light page, as before the
/// glass lane. And on a dark page the window's lane draws what the paper's frame draws in its face:
/// the latch never admits a dark scheme, so `--glass` changes a dark film's face and padding and
/// nothing the effects draw. Negative control: the same comparison on GitHub Light differs, and
/// that page's window lane latches its light.
#[test]
fn the_paper_lane_latches_no_light_and_a_dark_page_draws_the_same_in_either() {
    let at = [16, 1_408, LINE_TYPED_MS];
    for k in typing_frames(GITHUB_LIGHT, Lane::Paper, &at) {
        assert_eq!(
            (k.glass.light, k.glass.arms.as_str()),
            (Light::OFF, "-"),
            "{} ms",
            k.ms
        );
        assert_eq!(k.cell.1, gallery_cell().1);
    }
    let same = |look: Look| {
        let grounded = typing_frames(look, Lane::Glass, &at);
        let bare = typing_frames(look, Lane::Unlatched, &at);
        let lights: Vec<Light> = grounded.iter().map(|k| k.glass.light).collect();
        let same = grounded.iter().zip(&bare).all(|(g, b)| g.px == b.px);
        (same, lights)
    };
    let (alike, lights) = same(DARK);
    assert!(alike, "a dark page's window lane draws the paper frame");
    assert!(lights.iter().all(|&l| l == Light::OFF), "{lights:?}");
    let (alike, lights) = same(GITHUB_LIGHT);
    assert!(
        !alike,
        "control: a light page's window lane draws its own frame"
    );
    assert!(lights.iter().all(|l| l.glass()), "{lights:?}");
}

/// The glass draws on the pages the film's dims call glass pages (`glass_page=`, a light page
/// whose palette has the glass's headroom, `prism_glass::headroom`), and on no other: what
/// `lab trail-film --glass` asks of a film by that fact. Every film theme's typing scene in
/// [`Lane::Glass`], the line typed, draws the glass exactly where its page has the headroom.
/// Both sides are filmed: the pages with headroom draw it, and paper-grey, a light page whose
/// latch admits the glass's level (`ON|GLASS`) but whose grey ink leaves no headroom, draws
/// none, nor does any dark page, which never latches.
#[test]
fn the_glass_draws_on_the_glass_pages_and_on_no_other() {
    use aterm_effects::rainbow_kitty::prism_glass::headroom;
    let mut sides = [0usize; 2];
    for (name, look) in THEMES {
        let k = &typing_frames(look, Lane::Glass, &[LINE_TYPED_MS])[0];
        let page = headroom(look.fg, look.bg);
        assert_eq!(
            drew(k, "glass"),
            page,
            "{name}: glass page {page}, light={} arms={}",
            k.glass.light.bits(),
            k.glass.arms
        );
        sides[usize::from(page)] += 1;
        if name == "paper-grey" {
            assert!(
                k.glass.light.glass() && !page,
                "paper-grey latches, and has no headroom"
            );
        }
    }
    assert!(sides[0] > 0 && sides[1] > 0, "{sides:?}");
}

/// The rows of the typed row, from its cell's top (`dy`), whose pixels over the typed cells
/// (from the prompt's end to the caret) that the clean twin leaves `page` are vivid, chroma
/// [`GLASS_CHROMA`] or more, on half or more of them: the glass's body and its rail.
fn vivid_rows(k: &Kept, page: u32) -> Vec<i64> {
    let (cw, ch) = k.cell;
    let (row, caret) = k.caret;
    let y0 = k.top + row * ch;
    let h = k.px.len() / k.w;
    (-8..ch as i64 + 12)
        .filter(|&dy| {
            let Some(y) = y0.checked_add_signed(dy as isize).filter(|&y| y < h) else {
                return false;
            };
            let xs: Vec<usize> = (k.left + 2 * cw..k.left + caret * cw)
                .filter(|&x| k.clean[y * k.w + x] & 0x00FF_FFFF == page)
                .collect();
            let n = xs.len();
            let vivid = xs
                .into_iter()
                .filter(|&x| {
                    let f = k.px[y * k.w + x];
                    let c = [16u32, 8, 0].map(|sh| ((f >> sh) & 0xff) as i32);
                    c.iter().max().unwrap_or(&0) - c.iter().min().unwrap_or(&0) >= GLASS_CHROMA
                })
                .count();
            n > 0 && 2 * vivid >= n
        })
        .collect()
}

/// Where a real Retina window lays the glass on its typed row, measured on a debug aterm under
/// `lab instance --scale 2 --columns 72 --lines 14`, GitHub Light, rainbow, no pet, its line
/// typed (dims: cell 15 x 28, font_px 24, pad 24, pad_top 4, frame 1128 x 420): the body from
/// the cell's 5th pixel row to its last (dy 5 to 27), two page rows under the cell (28, 29), the
/// rail under them (30 to 35), and the deep glass from 69 px across, the typed row's 4th cell.
/// The rows are counted from the cell's foot (of a `ch` px cell, the page's `ch` and `ch + 1`,
/// the rail's `ch + 3` to `ch + 6`, the body's 8 to `ch - 1`), so the window laid round another
/// face's cell, the one [`Renderer::from_system`] loads off macOS, is read by the same rule.
/// `Err` names what of `k` is not where the window lays it.
fn laid_as_the_window_lays_it(k: &Kept, page: u32) -> Result<(), String> {
    let rows = vivid_rows(k, page);
    let has = |dy: i64| rows.contains(&dy);
    let (cw, ch) = k.cell;
    let foot = ch as i64;
    let mut off = Vec::new();
    if let Some(dy) = [foot, foot + 1].into_iter().find(|&dy| has(dy)) {
        off.push(format!(
            "dy {dy} is vivid, where the window leaves the page"
        ));
    }
    if let Some(dy) = (foot + 3..=foot + 6).find(|&dy| !has(dy)) {
        off.push(format!("dy {dy} is not, where the window lays its rail"));
    }
    if let Some(dy) = (8..foot).find(|&dy| !has(dy)) {
        off.push(format!("dy {dy} is not, where the window lays the glass"));
    }
    let y = k.top + k.caret.0 * ch + 15;
    let first = (0..k.w).find(|&x| {
        let f = k.px[y * k.w + x];
        let c = [16u32, 8, 0].map(|sh| ((f >> sh) & 0xff) as i32);
        c.iter().max().unwrap_or(&0) - c.iter().min().unwrap_or(&0) >= GLASS_CHROMA
            && aterm_render::linear_luminance(f & 0x00FF_FFFF) >= GLASS_FLOOR
    });
    if first.is_none_or(|x| x < k.left + 2 * cw || x > k.left + 4 * cw) {
        off.push(format!(
            "the deep glass starts at x {first:?}, not in the typed row's 3rd or 4th cell ({}..={})",
            k.left + 2 * cw,
            k.left + 4 * cw
        ));
    }
    if off.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} ms, vivid rows {rows:?}: {}",
            k.ms,
            off.join("; ")
        ))
    }
}

/// A glass film lays its light where the window lays it: its effects from the grid's corner in
/// the window's padding (`EffectsPipeline::set_chrome`, `set_chrome_pad_top`), so the glass and
/// its rail sit on the typed row's pixel rows as a real Retina window's do
/// ([`laid_as_the_window_lays_it`]), and its frame is the window's: its grid with a pad each side
/// across and its pads over and under it down, 1128 x 420 on SF Mono's cell. Negative control:
/// the same window whose effects are not told its padding lays them from the frame's corner, 24
/// px left of and 4 over the letters (the rail inside the cell, over its letters' feet), and
/// fails the reading.
#[test]
fn a_glass_film_lays_its_light_where_the_window_does() {
    let at = [2_208, LINE_TYPED_MS];
    let lit = typing_frames(GITHUB_LIGHT, Lane::Glass, &at);
    for k in &lit {
        laid_as_the_window_lays_it(k, GITHUB_LIGHT.bg).unwrap_or_else(|e| panic!("{e}"));
    }
    let mut k = lit.into_iter().next_back().expect("the line typed");
    let (rows, cols) = Take::Gallery(Scene::Typing).grid();
    window_frame(&mut k, rows);
    let (cw, ch) = k.cell;
    let frame = (
        usize::from(cols) * cw + 2 * RETINA_PAD,
        usize::from(rows) * ch + RETINA_PAD_TOP + RETINA_PAD,
    );
    assert_eq!((k.w, k.px.len() / k.w), frame, "the window's frame");
    if cfg!(target_os = "macos") {
        assert_eq!(frame, (1128, 420), "the window's frame on SF Mono's cell");
    }
    let unregistered = window_typing(GITHUB_LIGHT, RETINA_PX, true, false, &at);
    for k in &unregistered {
        let e = laid_as_the_window_lays_it(k, GITHUB_LIGHT.bg)
            .expect_err("control: laid from the frame's corner");
        let ch = k.cell.1;
        assert!(
            e.contains(&format!("dy {ch} is vivid"))
                || e.contains(&format!("dy {} is vivid", ch + 1)),
            "{e}"
        );
        assert!(e.contains("the deep glass starts at"), "{e}");
    }
}

/// A burst of typing under the glass yields the glass to its words as a window's word publish
/// does (`EffectsPipeline::yield_glass`, before the icons' shade): GitHub Light's burst in
/// [`Lane::Glass`] draws the yield (`yield:` in its arms) where the glass lies under its words.
/// Negative control: the same window unlatched draws no glass and so no yield.
#[test]
fn a_burst_under_the_glass_yields_it_to_the_words() {
    let at: Vec<u64> = (2_224..=2_320).step_by(16).collect();
    let take = Take::Gallery(Scene::Burst);
    let frames = |lane: Lane| {
        take_frames(
            take,
            GITHUB_LIGHT,
            |(rows, cols)| film_replay("rainbow", GITHUB_LIGHT, rows, cols, lane),
            &at,
        )
    };
    let lit = frames(Lane::Glass);
    let yielded = lit.iter().filter(|k| drew(k, "yield")).count();
    assert!(
        yielded > 0,
        "no yield in {:?}",
        lit.iter().map(|k| &k.glass.arms).collect::<Vec<_>>()
    );
    let bare = frames(Lane::Unlatched);
    assert!(bare.iter().all(|k| !drew(k, "glass") && !drew(k, "yield")));
}

/// The latch reads the face's own thin ink, as the window's does: SF Mono at 26 px lays `_`'s
/// stroke between two pixel rows, further toward the page than the glass is solved for
/// (`THIN_INK_SHARE`), so a light page latched over that face goes sheer and draws main's paper
/// band, no glass, on every frame. Negative control: the same window at the films' 24 px draws
/// the glass ([`a_retina_window_s_light_page_draws_the_glass_behind_the_typed_word`] reads it).
/// Held on macOS, the one platform whose [`Renderer::from_system`] loads SF Mono: the faces the
/// others load (JetBrains Mono on Linux, Iosevka Term NL on Windows) read thin ink 0 at 26 px.
#[cfg(target_os = "macos")]
#[test]
fn a_face_whose_thin_ink_is_over_the_glass_s_bound_draws_no_glass() {
    use aterm_effects::rainbow_kitty::prism_glass::colour::THIN_INK_SHARE;
    let at = [16, 1_408, LINE_TYPED_MS];
    for (px, sheer) in [(26.0, true), (RETINA_PX, false)] {
        for k in window_typing(GITHUB_LIGHT, px, true, true, &at) {
            let thin = k.glass.thin_ink.expect("the face's ink was read");
            assert_eq!(
                f64::from(thin) > THIN_INK_SHARE,
                sheer,
                "{px} px: thin ink {thin}"
            );
            let read = format!(
                "{px} px at {} ms: light={} arms={}",
                k.ms,
                k.glass.light.bits(),
                k.glass.arms
            );
            assert!(k.glass.light.on(), "latched: {read}");
            assert_eq!(k.glass.light.sheer(), sheer, "{read}");
            if sheer {
                assert!(!drew(&k, "glass"), "{read}");
            } else if k.ms == LINE_TYPED_MS {
                assert!(drew(&k, "glass"), "{read}");
            }
        }
    }
}
