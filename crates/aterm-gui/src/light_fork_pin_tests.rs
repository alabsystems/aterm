// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The host unreachability pin (`docs/design/PRISM-GLASS.md` §15.1 P0.9, §15.0.3): no dark
//! ground reaches the light fork through the app's own frame.
//!
//! Each dark ground ([`Ground::dark`]: the terminal's own colours with no theme admitted, which
//! the renderer's input carries as `COLOR_UNSET`; each dark built-in; each dark pack theme),
//! under each page a program lays over it ([`Page`]: none, DECSCNM, OSC 11 `#ffffff`, OSC 11
//! `#f6f7f9`), wherever it stands ([`Stand`]: a dark instance; a light instance's front tab
//! wearing it as its own theme; that tab split, its pane focused beside a light pane), is typed
//! on and drawn through each of the host's two entries to the cursor frame ([`Entry`]): the live
//! present, whose single-pane redraw reaches `cursor_frame_for_present` and which `lab
//! trail-diff`'s dump draws by, and the windowless capture path. Every frame must draw by main's
//! rule ([`breaks`]), which this file restates rather than calls, so a later change to the rule
//! fails here, and a tab or pane wearing the ground must lay the caret its dark twin lays
//! ([`fill_breaks`]). Each phase that adds a light symbol adds its "absent on every dark ground"
//! assert to [`light_symbols_absent`] (§15.0.3). The Settings preview's producer has its
//! companion pin in `settings_preview` (`the_preview_draws_every_dark_theme_by_mains_rule`).

use crate::cursor_glow::GlowConfig;
use crate::trail_rig::TrailRig;
use crate::{App, WindowId};
use aterm_effects::rainbow_kitty::{self as rk, ribbon::BedInkLut};
use aterm_effects::state_words::ArmsDrawn;
use std::time::{Duration, Instant};

/// The font the rig draws ([`TrailRig::new`]), as the dump draws it.
const FIXTURE_FONT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../aterm-render/tests/fixtures/jetbrains-mono.ttf"
);

/// The light instance a tab's own dark theme stands in.
const LIGHT_INSTANCE: &str = "Default Light";

/// A dark ground the pin sweeps.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Ground {
    /// No theme admitted: the terminal's own colours, which the renderer's input carries as
    /// `COLOR_UNSET` (§10.1: it counts as dark).
    Unset,
    /// A built-in theme, by name.
    Builtin(&'static str),
    /// A pack theme, installed from its text as `aterm theme install` lays it.
    Pack {
        id: &'static str,
        text: &'static str,
    },
}

impl Ground {
    /// Every dark ground: [`Self::Unset`], then each built-in and each pack theme whose
    /// background `aterm_render::theme_is_dark` reads dark.
    pub(crate) fn dark() -> Vec<Self> {
        let builtins = aterm_types::scheme::builtin_names()
            .into_iter()
            .map(Self::Builtin);
        let packs = aterm_theme_pack::themes().iter().map(|theme| Self::Pack {
            id: theme.id,
            text: theme.text,
        });
        let mut grounds = vec![Self::Unset];
        grounds.extend(builtins.chain(packs).filter(|ground| {
            ground
                .scheme()
                .is_some_and(|scheme| aterm_render::theme_is_dark(pack(scheme.background)))
        }));
        grounds
    }

    /// The scheme the ground's theme names; `None` for [`Self::Unset`].
    pub(crate) fn scheme(self) -> Option<aterm_types::scheme::ColorScheme> {
        match self {
            Self::Unset => None,
            Self::Builtin(name) => aterm_types::scheme::builtin(name),
            Self::Pack { text, .. } => aterm_types::scheme::parse_scheme_str(text).ok(),
        }
    }

    /// The selector a config or a tab's own theme names it by.
    const fn selector(self) -> Option<&'static str> {
        match self {
            Self::Unset => None,
            Self::Builtin(name) => Some(name),
            Self::Pack { id, .. } => Some(id),
        }
    }
}

impl std::fmt::Display for Ground {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.selector().unwrap_or("COLOR_UNSET"))
    }
}

/// What a program lays over the ground before the keys land.
#[derive(Clone, Copy, Debug)]
enum Page {
    Plain,
    /// DECSCNM: the page in reverse video.
    Decscnm,
    /// OSC 11 `#ffffff`.
    White,
    /// OSC 11 `#f6f7f9`, Default Light's page.
    Paper,
}

impl Page {
    const ALL: [Self; 4] = [Self::Plain, Self::Decscnm, Self::White, Self::Paper];

    const fn bytes(self) -> &'static [u8] {
        match self {
            Self::Plain => b"",
            Self::Decscnm => b"\x1b[?5h",
            Self::White => b"\x1b]11;#ffffff\x1b\\",
            Self::Paper => b"\x1b]11;#f6f7f9\x1b\\",
        }
    }
}

/// Where the dark ground stands.
#[derive(Clone, Copy, Debug)]
enum Stand {
    /// The instance's own theme.
    Instance,
    /// A light instance's front tab, wearing the ground as its own theme.
    TabTheme,
    /// That tab split, the ground's pane focused beside a pane in the light instance's theme.
    SplitPane,
}

impl Stand {
    const ALL: [Self; 3] = [Self::Instance, Self::TabTheme, Self::SplitPane];

    /// Whether `ground` can stand here: a tab's own theme always names one, so
    /// [`Ground::Unset`] stands only as an instance that admitted none.
    const fn takes(self, ground: Ground) -> bool {
        !matches!(
            (self, ground),
            (Self::TabTheme | Self::SplitPane, Ground::Unset)
        )
    }
}

/// The host's entry to the cursor frame a run draws through.
#[derive(Clone, Copy, Debug)]
enum Entry {
    /// The live present onto the rig's scripted CPU surface ([`TrailRig::frame`], as
    /// `lab trail-diff`'s dump draws), whose single-pane redraw takes `cursor_frame_for_present`
    /// and whose split redraw composes its panes first.
    Present,
    /// The windowless capture path (`App::windowless_frame_at`) on a window with no present
    /// target, as a `--no-windows` instance's `image` draws.
    Capture,
}

impl Entry {
    const ALL: [Self; 2] = [Self::Present, Self::Capture];
}

/// One frame's reads: the ground the focused pane shows and what the frame drew by.
#[derive(Clone, Copy)]
struct Drawn {
    /// The focused terminal's default `(fg, bg)`, swapped under DECSCNM: main's ground.
    ground: (u32, u32),
    /// The config the frame's glow drew by.
    cfg: Option<GlowConfig>,
    /// The bed's ink table the frame's ribbon drew by.
    bed: BedInkLut,
    /// The caret fill the frame laid, and the effects frame's own block fill the window presents.
    fill: (Option<u32>, Option<u32>),
    /// Which light arms drew.
    arms: ArmsDrawn,
    /// Whether the band was live: the ribbon held cells.
    live: bool,
}

/// `0x00RRGGBB`.
const fn pack(c: aterm_types::Rgb) -> u32 {
    (c.r as u32) << 16 | (c.g as u32) << 8 | c.b as u32
}

/// The glow config's part of main's rule (§10.1): the frame's polarity is the ground's,
/// `aterm_render::theme_is_dark` of its background, and its theme colours are the ground's own.
/// The broken asserts, by name.
pub(crate) fn config_breaks(cfg: &GlowConfig, (fg, bg): (u32, u32)) -> Vec<&'static str> {
    let mut out = Vec::new();
    if cfg.dark_theme != aterm_render::theme_is_dark(bg) {
        out.push("dark_theme");
    }
    if cfg.theme_fg != fg {
        out.push("theme_fg");
    }
    if cfg.theme_bg != bg {
        out.push("theme_bg");
    }
    out
}

/// Every assert of main's rule one frame `drawn` breaks, by name: empty when the light fork
/// stayed shut.
/// - The glow config ([`config_breaks`]).
/// - The bed's ink table is the one main's arm builds for the ground ([`BedInkLut::synced`] of
///   the config with the ground's polarity and colours).
/// - [`light_symbols_absent`].
///
/// The caret fill is held across frames instead ([`fill_breaks`]): on a focused window the host
/// lays the effects frame's block fill as it is (`window_cursor_fill`) and presents that one
/// colour as the window's block fill (`presented_block_fill`), so neither side of a frame can be
/// held to the other.
fn breaks(drawn: &Drawn) -> Vec<&'static str> {
    let Some(cfg) = drawn.cfg else {
        return vec!["no glow config"];
    };
    let mut out = config_breaks(&cfg, drawn.ground);
    let (fg, bg) = drawn.ground;
    let mains = GlowConfig {
        dark_theme: aterm_render::theme_is_dark(bg),
        theme_fg: fg,
        theme_bg: bg,
        ..cfg
    };
    if !same_table(
        &drawn.bed,
        &BedInkLut::synced(&rk::Config::from_glow(&mains, false)),
    ) {
        out.push("bed ink table");
    }
    light_symbols_absent(drawn, &mut out);
    out
}

/// The caret fill's part of main's rule, held to the dark twin: the same ground under the same
/// page drawn through the same entry as the instance's own theme ([`Stand::Instance`]), where no
/// light theme stands anywhere in the instance. A light instance's tab wearing the ground, or its
/// pane beside a light one, must lay the caret fill, and present the block fill, its twin does on
/// every frame. Each frame that differs, by index with both fills.
fn fill_breaks(twin: &[Drawn], drawn: &[Drawn]) -> Vec<String> {
    if twin.len() != drawn.len() {
        return vec![format!(
            "{} frames beside the twin's {}",
            drawn.len(),
            twin.len()
        )];
    }
    twin.iter()
        .zip(drawn)
        .enumerate()
        .filter(|(_, (t, d))| t.fill != d.fill)
        .map(|(n, (t, d))| {
            format!(
                "frame {n}: cursor fill {:06x?} beside its twin's {:06x?}",
                d.fill, t.fill
            )
        })
        .collect()
}

/// The light symbols, each absent on every dark ground (§15.0.3, §15.1 P0.9 "Grows with"): the
/// place each phase adds the assert for the symbol it adds.
/// - Today (P0.3): no light arm drew (`EffectsPipeline::light_arms_drawn`).
/// - Then, as they land: 1.1 `light == OFF`, and the frame light in WordGrid and WordPanes is
///   OFF; 1.3 the latch is off; 1.4 `prism_glass_solves == 0`; 1.10 `left_at` is empty; 2.1
///   `ink_top_captures == 0`; 2.2 `fold_at == None`; 2.5 the pet span is None; 3.1 no
///   LightPose; 3.5 no CaretHold; 3.6 `cursor_ink == None`; 4.3 no HeldBand; 4.5 no Afterimage.
fn light_symbols_absent(drawn: &Drawn, out: &mut Vec<&'static str>) {
    if !drawn.arms.is_empty() {
        out.push("light_arms_drawn");
    }
}

/// Whether two bed tables are the same table: the same key, inks and floors.
fn same_table(a: &BedInkLut, b: &BedInkLut) -> bool {
    a.key() == b.key()
        && a.lut() == b.lut()
        && a.seen()
            .iter()
            .zip(b.seen())
            .all(|(x, y)| x.to_bits() == y.to_bits())
}

/// The reads after a frame of `wid`.
fn read(app: &App, wid: WindowId, t0: Instant) -> Drawn {
    let mirror = app.front_terminal(wid).expect("a front terminal");
    let term = crate::term_lock(&mirror.term);
    let (fg, bg) = (
        pack(term.default_foreground()),
        pack(term.default_background()),
    );
    let ground = if term.modes().reverse_video() {
        (bg, fg)
    } else {
        (fg, bg)
    };
    drop(term);
    let ws = &app.windows[&wid];
    Drawn {
        ground,
        cfg: ws.fx.cursor().glow.last_cfg().copied(),
        bed: *ws.fx.bed_ink_lut(),
        fill: (
            ws.input_scratch.cursor_fill_override,
            ws.block_fill.map(|owned| owned.fill),
        ),
        arms: ws.fx.light_arms_drawn(),
        live: ws
            .fx
            .state_words(t0)
            .get("ribbon.cells.n")
            .is_some_and(|n| n > 0),
    }
}

/// The background of session `sid`'s terminal.
fn session_bg(app: &App, sid: u64) -> u32 {
    let session = app.pool.get(sid).expect("a live session");
    pack(crate::term_lock(&session.term).default_background())
}

/// One run: `ground` standing at `stand` under `page`, typed on and drawn through `entry`; the
/// rig and every frame's reads.
fn run(ground: Ground, page: Page, stand: Stand, entry: Entry) -> (TrailRig, Vec<Drawn>) {
    run_with(ground, page, stand, entry, "")
}

/// [`run`] with `extra` config lines beside the run's own (a stand that admits no config takes
/// none).
fn run_with(
    ground: Ground,
    page: Page,
    stand: Stand,
    entry: Entry,
    extra: &str,
) -> (TrailRig, Vec<Drawn>) {
    let mut rig = TrailRig::new(std::path::Path::new(FIXTURE_FONT)).expect("the rig");
    let t0 = rig.t0();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    if let Ground::Pack { id, text } = ground {
        rig.install_theme(id, text)
            .expect("the pack theme installs");
    }
    let mut toml = String::from(
        "trail_sounds = false\ncursor_trail_style = \"rainbow kitty\"\nmotion = \"full\"\n",
    );
    toml.push_str(extra);
    let instance = match stand {
        Stand::Instance => ground.selector(),
        Stand::TabTheme | Stand::SplitPane => Some(LIGHT_INSTANCE),
    };
    match instance {
        Some(theme) => {
            toml.push_str(&format!("theme = {theme:?}\n"));
            rig.configure(&toml, t0).expect("the config admits");
        }
        // No admission: the session keeps the terminal's own colours.
        None => rig.with_app(t0, |app, _| {
            app.config.cursor_trail_style = Some("rainbow kitty".into());
            app.config.motion = Some("full".into());
            app.config.trail_sounds = Some(false);
        }),
    }
    rig.grid(8, 40, t0).expect("the grid");
    if let Stand::SplitPane = stand {
        rig.split(t0).expect("the split");
        rig.focus_pane(1, t0).expect("the new pane takes the focus");
    }
    if let (Stand::TabTheme | Stand::SplitPane, Some(selector)) = (stand, ground.selector()) {
        rig.with_app(t0, |app, wid| {
            let sid = app.focused_session_id(wid).expect("a focused session");
            let (tx, rx) = std::sync::mpsc::channel();
            app.theme_control(
                crate::control_theme::ThemeCtlOp::SessionSet {
                    selector: selector.to_string(),
                },
                Some(sid),
                t0,
                tx,
            );
            let answer = rx.try_recv().expect("answered at once");
            assert!(answer.is_ok(), "{ground}: theme session set: {answer:?}");
        });
    }
    // The ground stands where the run says before a program touches it.
    rig.with_app(t0, |app, wid| {
        let sid = app.focused_session_id(wid).expect("a focused session");
        match ground.scheme() {
            Some(scheme) => assert_eq!(
                session_bg(app, sid),
                pack(scheme.background),
                "{ground} {stand:?}: the focused pane wears the ground"
            ),
            None => {
                let (rows, cols) = (app.windows[&wid].rows, app.windows[&wid].cols);
                let mut input = aterm_render::RenderInput::default();
                let session = app.pool.get(sid).expect("a live session");
                crate::term_lock(&session.term).cell_frame_into(
                    &mut input,
                    usize::from(rows),
                    usize::from(cols),
                );
                assert_eq!(
                    input.default_bg,
                    aterm_core::render::COLOR_UNSET,
                    "with no theme admitted the renderer's input carries no page"
                );
            }
        }
        if let Stand::SplitPane = stand {
            let other = app
                .pool
                .ids()
                .into_iter()
                .find(|&other| other != sid)
                .expect("the pane beside");
            assert!(
                !aterm_render::theme_is_dark(session_bg(app, other)),
                "{ground}: the pane beside is light"
            );
        }
    });
    if let Entry::Capture = entry {
        rig.with_app(t0, |app, wid| {
            app.windows.get_mut(&wid).expect("the window").present = None;
        });
    }
    rig.pty(page.bytes(), at(0));
    rig.pty(b"\x1b[4;1H$ ", at(0));
    let mut frames = Vec::new();
    let mut frame = |rig: &mut TrailRig, now: Instant| {
        match entry {
            Entry::Present => {
                rig.frame(now).expect("a presented frame");
            }
            Entry::Capture => {
                rig.with_app(now, |app, wid| app.windowless_frame_at(wid, now))
                    .expect("a windowless frame");
            }
        }
        frames.push(rig.with_app(now, |app, wid| read(app, wid, t0)));
    };
    frame(&mut rig, at(1));
    // Past the grid's resize hush, a key at a time with its echo, a frame on each and between.
    for (i, (key, echo)) in [("g", "g"), ("i", "i"), ("t", "t"), ("space", " ")]
        .into_iter()
        .enumerate()
    {
        let k = 700 + 90 * i as u64;
        rig.key(key, at(k)).expect("the key lands");
        rig.pty(echo.as_bytes(), at(k + 2));
        frame(&mut rig, at(k + 16));
        frame(&mut rig, at(k + 50));
    }
    // The band at rest, then retracted.
    frame(&mut rig, at(1400));
    frame(&mut rig, at(2600));
    (rig, frames)
}

/// Every dark ground, under every page, wherever it stands, through both of the host's entries
/// to the cursor frame: every frame draws by main's rule, and none reaches a light symbol.
#[test]
fn the_light_fork_is_unreachable_on_every_dark_ground() {
    let grounds = Ground::dark();
    assert!(
        grounds.iter().any(|g| matches!(g, Ground::Builtin(_)))
            && grounds.iter().any(|g| matches!(g, Ground::Pack { .. })),
        "the sweep holds dark built-ins and dark pack themes"
    );
    let (mut runs, mut frames, mut twinned) = (0, 0, 0);
    // Which polarities main's rule read: dark on a plain page, light under DECSCNM and OSC 11.
    let mut polarity = [false; 2];
    let mut failures = Vec::new();
    for &ground in &grounds {
        for page in Page::ALL {
            // Each entry's dark twin: the instance stand, which runs first.
            let mut twins: [Vec<Drawn>; Entry::ALL.len()] = Default::default();
            for stand in Stand::ALL.into_iter().filter(|stand| stand.takes(ground)) {
                for (e, entry) in Entry::ALL.into_iter().enumerate() {
                    let case = format!("{ground} {page:?} {stand:?} {entry:?}");
                    let (_, drawn) = run(ground, page, stand, entry);
                    runs += 1;
                    match stand {
                        Stand::Instance => twins[e].clone_from(&drawn),
                        Stand::TabTheme | Stand::SplitPane => {
                            twinned += drawn.iter().filter(|d| d.fill.0.is_some()).count();
                            for broke in fill_breaks(&twins[e], &drawn) {
                                failures.push(format!("{case}, {broke}"));
                            }
                        }
                    }
                    if !drawn.iter().any(|d| d.live) {
                        failures.push(format!("{case}: the band never lit"));
                    }
                    if !drawn.iter().any(|d| d.fill.0.is_some()) {
                        failures.push(format!("{case}: no frame laid a caret fill"));
                    }
                    for (n, d) in drawn.iter().enumerate() {
                        frames += 1;
                        if let Some(cfg) = d.cfg {
                            polarity[usize::from(cfg.dark_theme)] = true;
                        }
                        for broke in breaks(d) {
                            failures.push(format!("{case}, frame {n}: {broke}"));
                        }
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} breaks over {frames} frames:\n{}",
        failures.len(),
        failures.join("\n")
    );
    let tab_stands = Stand::ALL.len() - 1;
    assert_eq!(
        runs,
        Page::ALL.len() * Entry::ALL.len() * (1 + (grounds.len() - 1) * (1 + tab_stands)),
        "every ground ran under every page, at every stand that takes it, through both entries"
    );
    assert_eq!(polarity, [true, true], "main's rule read both polarities");
    assert!(twinned > 0, "caret fills were held to their twins");
}

/// The fill twin's negative control (§15.1 P0.9): a caret the host lays recoloured on both sides
/// of the frame, as a light caret arm would, passes every per-frame assert but fails its twin;
/// so does a real recolour through the cursor frame, a tab's caret pinned to its own colour.
#[test]
fn a_recoloured_caret_beside_its_dark_twin_fails_the_pin() {
    let ground = Ground::Builtin("Dracula");
    let (_, twin) = run(ground, Page::Plain, Stand::Instance, Entry::Present);
    let (_, tab) = run(ground, Page::Plain, Stand::TabTheme, Entry::Present);
    assert_eq!(
        fill_breaks(&twin, &tab),
        Vec::<String>::new(),
        "main's tab lays its twin's caret"
    );
    let lit = tab
        .iter()
        .position(|d| d.fill.0.is_some())
        .expect("a frame laid a caret fill");
    let mut recoloured = tab.clone();
    // The light caret's deep sage, laid and presented as one colour.
    recoloured[lit].fill = (Some(0x0091_A077), Some(0x0091_A077));
    assert_eq!(
        breaks(&recoloured[lit]),
        Vec::<&str>::new(),
        "the frame alone draws by every per-frame assert"
    );
    assert_eq!(
        fill_breaks(&twin, &recoloured).len(),
        1,
        "its twin catches it"
    );

    let (_, pinned) = run_with(
        ground,
        Page::Plain,
        Stand::TabTheme,
        Entry::Present,
        "cursor_color = \"#ff00ff\"\n",
    );
    assert!(
        pinned.iter().all(|d| d.fill.0 == d.fill.1),
        "the laid and the presented fill stay one colour"
    );
    assert!(
        !fill_breaks(&twin, &pinned).is_empty(),
        "a caret the cursor frame recolours fails its twin"
    );
}

/// The pin's negative control (§15.1 P0.9): on a dark ground, a frame forced onto the light
/// polarity fails it, through the real cursor frame and as a read with only `dark_theme`
/// forced; so does a light arm drawing.
#[test]
fn forcing_dark_theme_false_on_a_dark_ground_fails_the_pin() {
    let (mut rig, drawn) = run(
        Ground::Builtin("Dracula"),
        Page::Plain,
        Stand::Instance,
        Entry::Capture,
    );
    let live = *drawn.iter().rfind(|d| d.live).expect("the band lit");
    assert_eq!(
        breaks(&live),
        Vec::<&str>::new(),
        "main's frame holds the pin"
    );
    let cfg = live.cfg.expect("the frame's config");
    assert!(cfg.dark_theme, "Dracula is dark");

    // The cursor frame told a light page while the terminal's stays Dracula's.
    let t0 = rig.t0();
    let now = t0 + Duration::from_millis(2700);
    let told = rig.with_app(now, |app, wid| {
        let ws = &app.windows[&wid];
        let mut fx = crate::app_render::CursorFxInputs::sample_for_test(now);
        (fx.rows, fx.cols) = (usize::from(ws.rows), usize::from(ws.cols));
        fx.cur = Some((3, 6));
        (fx.default_fg, fx.default_bg) = (0x002B_2D33, 0x00F6_F7F9);
        assert!(app.cursor_frame(wid, fx).is_some(), "a presented tick");
        read(app, wid, t0)
    });
    assert!(
        !told.cfg.expect("the forced frame's config").dark_theme,
        "the forced frame drew light"
    );
    let broke = breaks(&told);
    assert!(
        broke.contains(&"dark_theme") && broke.contains(&"bed ink table"),
        "{broke:?}"
    );

    // Only `dark_theme` forced, with the table that config builds.
    let forced = GlowConfig {
        dark_theme: false,
        ..cfg
    };
    let flipped = Drawn {
        cfg: Some(forced),
        bed: BedInkLut::synced(&rk::Config::from_glow(&forced, false)),
        ..live
    };
    assert_eq!(breaks(&flipped), ["dark_theme", "bed ink table"]);

    let mut armed = live;
    armed
        .arms
        .note("glass", aterm_effects::state_words::Streams::GLOW_UNDER);
    assert_eq!(breaks(&armed), ["light_arms_drawn"]);
}
