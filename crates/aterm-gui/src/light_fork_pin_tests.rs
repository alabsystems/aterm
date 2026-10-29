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

use crate::cursor_glow::{GlowConfig, Light, Live};
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
    /// The pipeline's frame light at the frame's instant (`EffectsPipeline::frame_light`).
    light: Light,
    /// Whether the window's light latch admitted the frame (`TrailGround::last`).
    latched: bool,
    /// The window's light read outside its frame (`App::window_light`).
    window_light: Light,
    /// The lights the frame's word pass gave its panes (`EffectsPipeline::pane_word_light`), the
    /// first that is not [`Light::OFF`].
    pane_light: Light,
    /// The ribbon slabs laid as the light wedge since the last read
    /// (`aterm_render::ribbon_wedge_into`).
    wedges: usize,
    /// The ribbon slabs laid as the light glass since the last read
    /// (`aterm_render::ribbon_glass_into`).
    glass_slabs: usize,
    /// Whether the ribbon's last plan held the glass's geometry (`Ribbon::glass_planned`).
    glass_planned: bool,
    /// The glass's exits the ribbon ran since the last read: its envelopes, slab prices and
    /// pixel cadences (`rainbow_kitty::ribbon::take_glass_exits`).
    glass_exits: usize,
    /// The rows of the hand's structural set the ribbon holds as left (`Ribbon::glass_wraps`,
    /// each with its `left_at`), and whether it held the hand's row of a multi-row set
    /// (`Ribbon::wrap_hand`).
    glass_wraps: (usize, bool),
    /// Whether the band was live: the ribbon held cells.
    live: bool,
    /// The Prism Glass solve units the frame ran (`prism_glass::take_solves`).
    prism_glass_solves: usize,
    /// The light words the frame left (`EffectsPipeline::light_words`).
    light_words: usize,
    /// Whether the frame laid light-owned glass (`CursorGlow::glass_laid`).
    light_owned: bool,
    /// Whether a yield ever cut a frame's glass (`CursorGlow::last_yield`).
    yielded: bool,
    /// The light the glow's last tick began under, which a held park's flush draws by
    /// (`CursorGlow::tick_light`).
    tick_light: Light,
    /// The light each body engine's config carried: the rainbow block's, the momentum glow's,
    /// the comet's and the beam rod's (`CursorFx::engine_lights`).
    engine_lights: [Light; 4],
    /// The light the Rainbow Kitty's config was last built with (`CursorGlow::v2_light`).
    v2_light: Light,
    /// The light the rainbow block's settle probe latched (`CursorRainbow::settle_light`).
    settle_light: Option<Light>,
    /// The glass's height the ribbon's last sync gave it (`Ribbon::glass_px`).
    glass_px: f32,
    /// How many light-owned spans the ink ledger was ever handed
    /// (`CursorGlow::light_owned_noted`).
    ledger_owned: u64,
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
/// - P0.3: no light arm drew (`EffectsPipeline::light_arms_drawn`).
/// - 1.1: the frame's config carries [`Light::OFF`], and so does the frame light the words, the
///   pet and the yield read (`EffectsPipeline::frame_light`), each body engine's config
///   (`CursorFx::engine_lights`: the rainbow block's, the momentum glow's, the comet's, the beam
///   rod's), the Rainbow Kitty's (`CursorGlow::v2_light`) and the settle probe's latched one
///   (`CursorRainbow::settle_light`).
/// - 1.3: the window's latch admitted nothing (`TrailGround::last`), its light read outside the
///   frame is [`Light::OFF`] (`App::window_light`), and so is every pane's word light
///   (`EffectsPipeline::pane_word_light`) and the light a held park's flush draws by
///   (`CursorGlow::tick_light`).
/// - 1.4: no Prism Glass solve unit ran (`prism_glass_solves == 0`,
///   `rainbow_kitty::prism_glass::take_solves`).
/// - 1.5: the bed draws by main's table, not the glass ([`BedInkLut::glass_lit`]), and the
///   frame leaves no light words (`EffectsPipeline::light_words`: the bed's glass, the palettes).
/// - 1.6: no ribbon slab was laid as the light wedge
///   (`aterm_render::test_counters::take_wedge_slabs`).
/// - 1.7: no ribbon slab was laid as the light glass
///   (`aterm_render::test_counters::take_glass_slabs`), and the ribbon planned no glass reach
///   or hard step (`Ribbon::glass_planned`), and its glass has no height (`Ribbon::glass_px`).
///   The page's sheer bit is a [`Light`] bit, so the 1.1 asserts hold it too.
/// - 1.8: the ribbon folded no glass envelope, priced no glass slab and stepped no glass
///   cadence (`glass_exits == 0`, `rainbow_kitty::ribbon::take_glass_exits`).
/// - 1.9: no glass was laid light-owned, the letter guard's to leave and the yield's to cut,
///   by the yield's record (`CursorGlow::glass_laid`) or the ink ledger's own
///   (`CursorGlow::light_owned_noted`), and no yield cut a frame (`CursorGlow::last_yield`).
/// - 1.10: the ribbon holds no `left_at` and no hand's row of a multi-row set
///   (`Ribbon::glass_wraps`, `Ribbon::wrap_hand`).
/// - 1.11: the Settings preview's light, which no frame of the app's carries, is asserted
///   [`Light::OFF`] by the companion pin (`settings_preview`'s `preview_breaks`).
/// - Then, as they land: 2.1 `ink_top_captures == 0`; 2.2 `fold_at == None`; 2.5 the pet span
///   is None; 3.1 no LightPose; 3.5 no CaretHold; 3.6 `cursor_ink == None`; 4.3 no HeldBand;
///   4.5 no Afterimage.
fn light_symbols_absent(drawn: &Drawn, out: &mut Vec<&'static str>) {
    if !drawn.arms.is_empty() {
        out.push("light_arms_drawn");
    }
    if drawn.cfg.is_some_and(|cfg| cfg.light != Light::OFF) {
        out.push("light");
    }
    if drawn.light != Light::OFF {
        out.push("frame_light");
    }
    if drawn.wedges > 0 {
        out.push("ribbon_wedge_into");
    }
    if drawn.glass_slabs > 0 {
        out.push("ribbon_glass_into");
    }
    if drawn.glass_planned {
        out.push("glass plan");
    }
    if drawn.glass_exits != 0 {
        out.push("glass exits");
    }
    if drawn.glass_wraps.0 != 0 {
        out.push("left_at");
    }
    if drawn.glass_wraps.1 {
        out.push("wrap_hand");
    }
    if drawn.prism_glass_solves != 0 {
        out.push("prism_glass_solves");
    }
    if drawn.latched {
        out.push("latch");
    }
    if drawn.window_light != Light::OFF {
        out.push("window_light");
    }
    if drawn.pane_light != Light::OFF {
        out.push("pane_word_light");
    }
    if drawn.bed.glass_lit() {
        out.push("bed glass");
    }
    if drawn.light_words != 0 {
        out.push("light_words");
    }
    if drawn.light_owned {
        out.push("light_owned");
    }
    if drawn.yielded {
        out.push("yield");
    }
    if drawn.tick_light != Light::OFF {
        out.push("tick_light");
    }
    for (light, engine) in drawn.engine_lights.iter().zip([
        "rainbow light",
        "momentum light",
        "comet light",
        "beamrod light",
    ]) {
        if *light != Light::OFF {
            out.push(engine);
        }
    }
    if drawn.v2_light != Light::OFF {
        out.push("rk light");
    }
    if drawn.settle_light.is_some_and(|light| light != Light::OFF) {
        out.push("settle light");
    }
    if drawn.glass_px != 0.0 {
        out.push("glass_px");
    }
    if drawn.ledger_owned != 0 {
        out.push("ledger light_owned");
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

/// The reads after the frame of `wid` at `now`.
fn read(app: &App, wid: WindowId, t0: Instant, now: Instant) -> Drawn {
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
        light: ws.fx.frame_light(now),
        latched: ws.trail_ground.last().on,
        window_light: app.window_light(wid),
        pane_light: app
            .pool
            .ids()
            .into_iter()
            .map(|sid| ws.fx.pane_word_light(now, sid))
            .find(|&light| light != Light::OFF)
            .unwrap_or(Light::OFF),
        wedges: aterm_render::test_counters::take_wedge_slabs(),
        glass_slabs: aterm_render::test_counters::take_glass_slabs(),
        glass_planned: ws
            .fx
            .cursor()
            .glow
            .v2_ribbon()
            .is_some_and(aterm_effects::rainbow_kitty::ribbon::Ribbon::glass_planned),
        glass_exits: rk::ribbon::take_glass_exits(),
        glass_wraps: ws.fx.cursor().glow.v2_ribbon().map_or((0, false), |rib| {
            (rib.glass_wraps().len(), rib.wrap_hand().is_some())
        }),
        live: ws
            .fx
            .state_words(t0)
            .get("ribbon.cells.n")
            .is_some_and(|n| n > 0),
        prism_glass_solves: rk::prism_glass::take_solves(),
        light_words: ws.fx.light_words(t0).words().len(),
        light_owned: !ws.fx.cursor().glow.glass_laid().is_empty(),
        yielded: ws.fx.cursor().glow.last_yield().is_some(),
        tick_light: ws.fx.cursor().glow.tick_light(),
        engine_lights: ws.fx.cursor().engine_lights(),
        v2_light: ws.fx.cursor().glow.v2_light(),
        settle_light: ws.fx.cursor().rainbow.settle_light(),
        glass_px: ws
            .fx
            .cursor()
            .glow
            .v2_ribbon()
            .map_or(0.0, aterm_effects::rainbow_kitty::ribbon::Ribbon::glass_px),
        ledger_owned: ws.fx.cursor().glow.light_owned_noted(),
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
        frames.push(rig.with_app(now, |app, wid| read(app, wid, t0, now)));
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
/// to the cursor frame: every frame draws by main's rule, and none reaches a light symbol. Each
/// ground under each page runs on a core of its own, its stands in order (its twin first), each
/// run on a rig of its own.
#[test]
fn the_light_fork_is_unreachable_on_every_dark_ground() {
    let grounds = Ground::dark();
    for theme in aterm_theme_pack::themes() {
        if let Err(err) = aterm_types::scheme::parse_scheme_str(theme.text) {
            panic!("pack theme {} parses: {err:?}", theme.id);
        }
    }
    assert!(
        grounds.iter().any(|g| matches!(g, Ground::Builtin(_)))
            && grounds.iter().any(|g| matches!(g, Ground::Pack { .. })),
        "the sweep holds dark built-ins and dark pack themes"
    );
    // COLOR_UNSET, then every dark scheme.
    assert!(
        grounds.len() > 37,
        "{} dark schemes swept",
        grounds.len() - 1
    );
    let cases: Vec<(Ground, Page)> = grounds
        .iter()
        .flat_map(|&ground| Page::ALL.map(|page| (ground, page)))
        .collect();
    let swept = aterm_spec::check::each(&cases, |&(ground, page)| {
        let (mut runs, mut frames, mut twinned) = (0, 0, 0);
        // Which polarities main's rule read: dark on a plain page, light under DECSCNM and OSC 11.
        let mut polarity = [false; 2];
        let mut failures = Vec::new();
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
        (runs, frames, twinned, polarity, failures)
    });
    let (mut runs, mut frames, mut twinned) = (0, 0, 0);
    let mut polarity = [false; 2];
    let mut failures = Vec::new();
    for (r, f, t, read, broke) in swept {
        (runs, frames, twinned) = (runs + r, frames + f, twinned + t);
        polarity = [polarity[0] || read[0], polarity[1] || read[1]];
        failures.extend(broke);
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

/// The light reads' wiring: on a light configured scheme the latch admits, so the reads the pin
/// holds to [`Light::OFF`] on every dark ground carry its light there, each read from the code
/// that draws by it. The glass's own symbols (`glass_px`, the ledger's light-owned spans) stay
/// off while the live phase is [`Live::Off`].
#[test]
fn the_light_reads_carry_a_light_schemes_light() {
    let (_, drawn) = run(
        Ground::Builtin(LIGHT_INSTANCE),
        Page::Plain,
        Stand::Instance,
        Entry::Present,
    );
    let broke: std::collections::BTreeSet<&str> = drawn.iter().flat_map(breaks).collect();
    for read in [
        "light",
        "tick_light",
        "rainbow light",
        "momentum light",
        "comet light",
        "beamrod light",
        "rk light",
    ] {
        assert!(broke.contains(read), "{read} carried the light: {broke:?}");
    }
}

/// The pin's negative control (§15.1 P0.9): on a dark ground, a frame forced onto the light
/// polarity fails it, through the real cursor frame and as a read with only `dark_theme`
/// forced; so does a light arm drawing, a bed drawn by the glass, light words, a latched light
/// in the frame's config or its frame light, an admitting latch, a light read outside the frame
/// or given a pane, and each light symbol of [`light_symbols_absent`] set alone.
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
        read(app, wid, t0, now)
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

    // The bed a glass frame on Default Light's page syncs, and the light words a frame leaves.
    let glass = GlowConfig {
        dark_theme: false,
        theme_fg: 0x002B_2D33,
        theme_bg: 0x00F6_F7F9,
        light: Light::at(Live::Glass),
        ..cfg
    };
    let glassed = Drawn {
        bed: BedInkLut::synced(&rk::Config::from_glow(&glass, false)),
        ..live
    };
    assert!(
        glassed.bed.glass_lit(),
        "fixture: Default Light's glass ships"
    );
    assert_eq!(breaks(&glassed), ["bed ink table", "bed glass"]);
    let worded = Drawn {
        light_words: 1,
        ..live
    };
    assert_eq!(breaks(&worded), ["light_words"]);
    assert_eq!(
        breaks(&Drawn {
            light_owned: true,
            ..live
        }),
        ["light_owned"]
    );
    assert_eq!(
        breaks(&Drawn {
            yielded: true,
            ..live
        }),
        ["yield"]
    );

    // A latched light, in the frame's config or as the frame light, even at `Live::Off`.
    let latched = Drawn {
        cfg: Some(GlowConfig {
            light: Light::at(Live::Off),
            ..cfg
        }),
        ..live
    };
    assert_eq!(breaks(&latched), ["light"]);
    let lit = Drawn {
        light: Light::at(Live::Glass),
        ..live
    };
    assert_eq!(breaks(&lit), ["frame_light"]);
    assert_eq!(
        breaks(&Drawn {
            latched: true,
            ..live
        }),
        ["latch"]
    );
    assert_eq!(
        breaks(&Drawn {
            window_light: Light::at(Live::Off),
            ..live
        }),
        ["window_light"]
    );
    assert_eq!(
        breaks(&Drawn {
            pane_light: Light::at(Live::Glass),
            ..live
        }),
        ["pane_word_light"]
    );
    assert_eq!(
        breaks(&Drawn {
            glass_exits: 1,
            ..live
        }),
        ["glass exits"]
    );
    assert_eq!(
        breaks(&Drawn {
            glass_wraps: (1, false),
            ..live
        }),
        ["left_at"]
    );
    assert_eq!(
        breaks(&Drawn {
            glass_wraps: (0, true),
            ..live
        }),
        ["wrap_hand"]
    );
    let off = Light::at(Live::Off);
    assert_eq!(
        breaks(&Drawn {
            tick_light: off,
            ..live
        }),
        ["tick_light"]
    );
    for (engine, name) in [
        "rainbow light",
        "momentum light",
        "comet light",
        "beamrod light",
    ]
    .into_iter()
    .enumerate()
    {
        let mut engine_lights = [Light::OFF; 4];
        engine_lights[engine] = off;
        assert_eq!(
            breaks(&Drawn {
                engine_lights,
                ..live
            }),
            [name]
        );
    }
    assert_eq!(
        breaks(&Drawn {
            v2_light: off,
            ..live
        }),
        ["rk light"]
    );
    assert_eq!(
        breaks(&Drawn {
            settle_light: Some(off),
            ..live
        }),
        ["settle light"]
    );
    assert_eq!(
        breaks(&Drawn {
            settle_light: Some(Light::OFF),
            ..live
        }),
        Vec::<&str>::new(),
        "a probe latched dark is main's"
    );
    assert_eq!(
        breaks(&Drawn {
            glass_px: 1.0,
            ..live
        }),
        ["glass_px"]
    );
    assert_eq!(
        breaks(&Drawn {
            ledger_owned: 1,
            ..live
        }),
        ["ledger light_owned"]
    );
}
