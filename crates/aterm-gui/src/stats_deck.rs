// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE STATS DECK: the three bars, Global, aterm and Tab, drawn as pixel
//! graphics under the titlebar (`docs/DESIGN-system-awareness-2026-10-16.md`
//! §12.2; on by default since the owner's 2026-10-19 "make sure that this
//! feature is enabled on BY DEFAULT so that I can SEE IT").
//!
//! The Deck is a band of the window's chrome headroom: [`deck_px`] device
//! pixels added to `MetricsView::head` on a window with glass, so every
//! geometry law that reads the headroom (the grid fit, the frame size, the
//! pointer map, the effects box) counts it with no list of its own. A headless
//! window never has one. Under macOS's titlebar it is the head's foot; where
//! the strip is the window's band (Linux, Windows) it is the head's top, and
//! the band keeps its whole height under it (`App::deck_rows_px`). It is
//! painted from [`crate::widget::DrawPrim`]s through the shared tray
//! rasterizer (`crate::stats_paint`), identical on the CPU and GPU paths and
//! in `image` captures, into the window's
//! paint-only stats card, which is folded under any other card
//! (`App::fold_stats_deck`). The compact form, the sill
//! (`crate::stats_sill`), takes the same card in the window's bottom pad and
//! no terminal row; the lens (`crate::stats_lens`) is the close-up over
//! either.
//!
//! What it draws, one grammar for tiles and bars (§12.1):
//! * **Cores**: one square per core, grouped by cluster as the OS names them
//!   (each cluster labelled with its initial over its count, the GPU tank
//!   labelled the same way beside them), fill height and the load ramp for
//!   the level, a dotted ghost at the 20 s average, a warm sliver and a notch
//!   when rising, a pale band when falling, a warm halo round what is rising,
//!   a crisp or feathered edge for steady or jittery.
//! * **Bars**: per scope, each with its key swatch (the Global bar's is the
//!   load ramp, aterm's and the tab's their scope inks, so the label is the
//!   legend), CPU on one capacity scale (so Tab ≤ aterm ≤ Global reads as
//!   geometry, with a capacity bracket that fills when the machine is
//!   pinned), a two-minute sparkline, memory on one physical scale, the
//!   figures in tabular type and the words in clauses that fold whole from the
//!   end, never cut mid-word.
//!
//! Rich does not mean animated (§12.5, O34): nothing here schedules a frame.
//! A new snapshot re-lays the prims; positions are whole device pixels, so a
//! frame is presented only when a drawn pixel moves (`WindowState::deck_memo`).

use aterm_vitals::Motion;
use aterm_vitals::ink::{self, Rgb, VitalsInks};
use aterm_vitals::model::{CpuView, GpuView, ScopeView, Snapshot, TabView};
use aterm_vitals::trend::TrendView;
use aterm_vitals::words;

use crate::stats_paint::{
    Bar, Clause, Hit, Instrument, Mark, Painter, StatsMark, WellMark, a, panel,
};
use crate::widget::{DrawPrim, rgba};

/// The Deck's height in points: three 16 pt bars, 3 pt apart, 4 pt margins
/// (124 device px at 2×, §12.2).
pub(crate) const DECK_PT: f64 = 62.0;

/// Which form the stats bars take (`[sys] stats_bars`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum StatsBars {
    /// The three bars at full size under the titlebar (the default).
    Deck,
    /// The three bars as light lanes in the window's bottom pad: no terminal
    /// row, ever (`crate::stats_sill`).
    Sill,
    /// No bars.
    Off,
}

impl StatsBars {
    /// Every form, in the View ▸ Stats Bars menu's order.
    pub(crate) const ALL: [Self; 3] = [Self::Deck, Self::Sill, Self::Off];

    /// Parse the config value: `"deck"` (default), `"sill"`, `"off"`.
    /// Unknown values read as the default, so the bars stay visible.
    pub(crate) fn parse(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            Some(v) if v.eq_ignore_ascii_case("off") || v.eq_ignore_ascii_case("false") => {
                Self::Off
            }
            Some(v) if v.eq_ignore_ascii_case("sill") => Self::Sill,
            _ => Self::Deck,
        }
    }

    /// The config word for this form.
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Deck => "deck",
            Self::Sill => "sill",
            Self::Off => "off",
        }
    }

    /// The form's name in a menu.
    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::Deck => "Deck",
            Self::Sill => "Sill",
            Self::Off => "Off",
        }
    }

    /// The form a config word names, or `None` for a word that is not one.
    pub(crate) fn from_word(word: &str) -> Option<Self> {
        let word = word.trim();
        Self::ALL
            .into_iter()
            .find(|f| f.word().eq_ignore_ascii_case(word))
    }

    /// Whether any bars show.
    pub(crate) const fn shown(self) -> bool {
        !matches!(self, Self::Off)
    }

    /// Whether the form takes room from the grid: the Deck's band in the
    /// head. A change across this line re-grids every window; the sill and
    /// off take none, so a change between them only repaints (§12.2).
    pub(crate) const fn takes_room(self) -> bool {
        matches!(self, Self::Deck)
    }
}

/// The Deck's height in device pixels at `scale`, or 0 when the Deck is not
/// the form (the sill and off take no headroom).
pub(crate) fn deck_px(form: StatsBars, scale: f64) -> usize {
    match form {
        f if f.takes_room() && scale.is_finite() && scale > 0.0 => {
            (DECK_PT * scale).round() as usize
        }
        _ => 0,
    }
}

/// An attached window's chrome headroom in device pixels: the titlebar band
/// (`head_pts`, measured from AppKit or the synthetic strip band) plus the
/// Deck. Every site that derives `MetricsView::head` for a window with glass
/// goes through here, so the Deck is counted by the grid fit, the frame size
/// and the pointer map alike; `head_pts` keeps only the titlebar's part.
pub(crate) fn chrome_head_px(head_pts: f64, scale: f64, form: StatsBars) -> usize {
    (head_pts * scale).round().max(0.0) as usize + deck_px(form, scale)
}

/// Headless stats seam (development only, [`aterm_types::dev_seam!`]):
/// `$ATERM_STATS_DECK_HEADLESS=1` draws the bars on a window with no glass
/// (the Deck when its headroom holds it, `$ATERM_HEADROOM_PX`; the sill in
/// the bottom pad), so a lab instance's `image` shows them. Always off in a
/// shipped binary. Parsed once.
pub(crate) fn deck_on_headless() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        aterm_types::dev_seam!("ATERM_STATS_DECK_HEADLESS")
            .is_some_and(|v| v.to_str().is_some_and(|v| v.trim() == "1"))
    })
}

/// Whether `ws` draws the stats bars: a window with glass, or any window
/// under the headless seam ([`deck_on_headless`]) or in a unit test that
/// asks for it ([`tests::attach_stats`]).
pub(crate) fn stats_drawn_on(ws: &crate::WindowState) -> bool {
    ws.os_window.is_some() || deck_on_headless() || tests_attached()
}

#[cfg(test)]
thread_local! {
    /// A unit test's stand-in for glass: its windows draw the bars.
    static TESTS_ATTACHED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn tests_attached() -> bool {
    TESTS_ATTACHED.with(std::cell::Cell::get)
}

#[cfg(not(test))]
const fn tests_attached() -> bool {
    false
}

/// The inks solved on a ground, its words and its accent
/// ([`ink::vitals_inks`], a few microseconds to tens of them on a light
/// ground), remembered for the last few grounds: every frame's fingerprint
/// and every pointer event over the bars asks, and the grounds change only
/// with the theme.
pub(crate) fn cached_inks(ground: Rgb, value: Rgb, accent: Rgb) -> VitalsInks {
    type Key = (Rgb, Rgb, Rgb);
    thread_local! {
        static SOLVED: std::cell::RefCell<Vec<(Key, VitalsInks)>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }
    let key = (ground, value, accent);
    SOLVED.with(|cache| {
        if let Some((_, inks)) = cache.borrow().iter().find(|(k, _)| *k == key) {
            return *inks;
        }
        let inks = ink::vitals_inks(ground, value, accent);
        let mut cache = cache.borrow_mut();
        if cache.len() >= 4 {
            cache.remove(0);
        }
        cache.push((key, inks));
        inks
    })
}

impl crate::App {
    /// The stats bars' form from the live config.
    pub(crate) fn stats_bars_form(&self) -> StatsBars {
        self.config.stats_bars_or_default()
    }

    /// The Deck's height in window `wid`'s head when it draws there
    /// ([`Self::deck_frame`]'s Deck): 0 under the sill or off, on a window
    /// that draws no bars, or one whose head cannot hold it.
    pub(crate) fn deck_head_px(&self, wid: crate::WindowId) -> usize {
        let form = self.stats_bars_form();
        let Some(ws) = self.windows.get(&wid) else {
            return 0;
        };
        if ws.cols == 0 || !stats_drawn_on(ws) {
            return 0;
        }
        match deck_px(form, ws.scale) {
            deck if deck > 0 && ws.metrics.head >= deck => deck,
            _ => 0,
        }
    }

    /// The Deck's rows in window `wid`'s frame, `[top, bottom)` device px,
    /// while it draws in the head ([`Self::deck_head_px`]). Under macOS's
    /// titlebar it ends where the grid's top pad starts. Where the strip is
    /// the window's band (Linux, Windows), the band is the head's synthetic
    /// part, the top pad and the strip's own rows, which the grid's first row
    /// closes: the Deck takes the head's top, and the band keeps its whole
    /// height under it ([`Self::strip_band_top_px`]), its cards, its `+` and
    /// the window's buttons with it. Ending at the head there put the Deck
    /// over the band's middle, where the band centres its cards (the lane's
    /// `proof-20261019-174123`).
    pub(crate) fn deck_rows_px(&self, wid: crate::WindowId) -> Option<std::ops::Range<usize>> {
        let deck = self.deck_head_px(wid);
        if deck == 0 {
            return None;
        }
        let head = self.windows.get(&wid)?.metrics.head;
        let top = if crate::tab_bar::STRIP_IS_CHROME_BAND {
            0
        } else {
            head - deck
        };
        Some(top..top + deck)
    }

    /// Where window `wid`'s strip band starts above its first row, in device
    /// px: the top pad and the head, less the Deck where the Deck takes the
    /// head's top (the strip is the window's band: Linux, Windows; see
    /// [`Self::deck_frame`]). The band's raster lifts its canvas by this and
    /// centres its cards in what it spans, so the band keeps its height and
    /// the Deck never covers it.
    pub(crate) fn strip_band_top_px(&self, wid: crate::WindowId) -> usize {
        let top = self.win_pad_top(wid) + self.win_head(wid);
        if crate::tab_bar::STRIP_IS_CHROME_BAND {
            top.saturating_sub(self.deck_head_px(wid))
        } else {
            top
        }
    }
}

/// Where a stats surface is drawn: its card's size in device pixels, the
/// window's side pad (so the bars align with the grid), the scale and the
/// chrome text size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DeckGeom {
    /// Card width (the frame's width).
    pub(crate) w: f32,
    /// Card height ([`deck_px`], or the bottom pad for the sill).
    pub(crate) h: f32,
    /// The window's side pad.
    pub(crate) pad: f32,
    /// Device pixels per point.
    pub(crate) scale: f32,
    /// The terminal font's pixel size (the chrome text base).
    pub(crate) font_px: f32,
}

/// What a stats surface shows: the snapshot, which tab is this window's, and
/// the inks solved for its ground.
pub(crate) struct DeckInput<'a> {
    /// The latest snapshot, `None` before the first.
    pub(crate) snap: Option<&'a Snapshot>,
    /// This window's active tab's key (its session id).
    pub(crate) tab_key: Option<u64>,
    /// Its label, `Tab 2`.
    pub(crate) tab_label: String,
    /// How many tabs this window holds (the sill draws the Tab lane at 2+;
    /// the Deck says "the only tab" at 1).
    pub(crate) tab_count: usize,
    /// Inks for the ground.
    pub(crate) inks: VitalsInks,
    /// A hairline between the Deck and the grid.
    pub(crate) seam: Rgb,
    /// Differentiate Without Color (System Settings ▸ Accessibility).
    pub(crate) dwc: bool,
    /// The instance's deviations, worst first (⊘ > ☠ > ◔ > ◇ > stopped).
    pub(crate) marks: Vec<StatsMark>,
    /// The sill's plate behind each instrument, as an alpha, while the floor
    /// is translucent (`background_opacity` under 1, or a wallpaper): 0.92,
    /// or opaque under Reduce Transparency (§12.2 "Plate"). `None` on an
    /// opaque floor and for the Deck, whose band is opaque.
    pub(crate) plate: Option<u8>,
}

/// The bars' label, figures and words for one scope row.
struct Row<'a> {
    bar: Bar,
    label: String,
    scope: Option<&'a ScopeView>,
    cpu_ink: Rgb,
    mem_ink: Rgb,
    cpu_fig: String,
    mem_fig: String,
    clauses: Vec<Clause>,
}

/// Lay the Deck out and paint it. Pure: the same input gives the same prims,
/// at whole device pixels.
#[cfg(test)]
pub(crate) fn deck_prims(g: &DeckGeom, input: &DeckInput<'_>) -> Vec<DrawPrim> {
    deck_paint(g, input).0
}

/// The core tiles by cluster and the GPU tank, from `x`; returns where the
/// block ends.
#[allow(clippy::too_many_arguments)]
fn core_block(
    p: &mut Painter<'_>,
    cpu: &CpuView,
    gpu: Option<&GpuView>,
    x0: f32,
    top: f32,
    inner_h: f32,
    budget: f32,
    hits: &mut Vec<Hit>,
) -> f32 {
    let inks = *p.inks;
    let gap = p.px(2.5);
    let lab_pad = p.px(4.0);
    let label_ws: Vec<f32> = cpu
        .clusters
        .iter()
        .map(|c| {
            p.label_w(&c.short, true, true)
                .max(p.figure_w(&format!("\u{00D7}{}", c.cores.len()), true))
                + lab_pad
        })
        .collect();
    let gpu_lw = p.label_w("GPU", true, true).max(p.figure_w("100%", true)) + lab_pad;
    // Rows of tiles: two (32 px tiles and up at 2×) unless the cores would
    // take more than their share of the width; then more, smaller rows.
    // Each cluster fills row-major, so no cluster is ragged.
    let mut rows = if cpu.total() <= 2 { 1usize } else { 2 };
    let (tile, cols) = loop {
        let t = ((inner_h - gap * (rows as f32 - 1.0)) / rows as f32).floor();
        let cols: Vec<usize> = cpu
            .clusters
            .iter()
            .map(|c| c.cores.len().div_ceil(rows).max(1))
            .collect();
        let tank_w = (t * 0.62).round().max(p.px(6.0));
        let need: f32 = cpu
            .clusters
            .iter()
            .zip(cols.iter())
            .zip(label_ws.iter())
            .map(|((_, &k), &lw)| lw + k as f32 * (t + gap) + gap * 2.0)
            .sum::<f32>()
            + gpu.map_or(0.0, |_| gpu_lw + tank_w + gap * 2.0);
        if need <= budget || rows >= 8 || t <= p.px(5.0) {
            break (t, cols);
        }
        rows += 1;
    };
    let block_h = rows as f32 * tile + (rows as f32 - 1.0) * gap;
    let top = top + ((inner_h - block_h) / 2.0).floor();
    let name_base =
        crate::tray_raster::row_baseline(top, tile.max(p.small_px.get()), p.small_px.get());
    let count_base = crate::tray_raster::row_baseline(
        top + block_h - tile.max(p.small_px.get()),
        tile.max(p.small_px.get()),
        p.small_px.get(),
    );
    let (name_base, count_base) = if rows == 1 {
        let half = block_h / 2.0;
        (
            crate::tray_raster::row_baseline(top, half, p.small_px.get()),
            crate::tray_raster::row_baseline(top + half, half, p.small_px.get()),
        )
    } else {
        (name_base, count_base)
    };
    let mut x = x0;
    for ((cluster, &k_cols), &lw) in cpu.clusters.iter().zip(cols.iter()).zip(label_ws.iter()) {
        let start = x;
        p.label(x, name_base, cluster.short.clone(), inks.words, true, true);
        p.figure(
            x,
            count_base,
            format!("\u{00D7}{}", cluster.cores.len()),
            inks.words2,
            true,
        );
        x += lw;
        let used_rows = cluster.cores.len().div_ceil(k_cols).max(1);
        let y_off = (((rows - used_rows) as f32) * (tile + gap) / 2.0).floor();
        for (i, &core) in cluster.cores.iter().enumerate() {
            let (col, row) = (i % k_cols, i / k_cols);
            let tx = x + col as f32 * (tile + gap);
            let ty = top + y_off + row as f32 * (tile + gap);
            match cpu.cores.get(core) {
                Some(view) => {
                    p.tile(
                        tx,
                        ty,
                        tile,
                        Mark::of(&view.load, 1.0, words::LOAD_GATE),
                        gap * 2.0,
                    );
                }
                None => p.tile_unmeasured(tx, ty, tile, tile),
            }
        }
        x += k_cols as f32 * (tile + gap) + gap;
        hits.push(Hit {
            rect: (start, top, x - start, block_h),
            instrument: Instrument::Cores,
            bar: Bar::Global,
        });
    }
    if let Some(gpu) = gpu {
        // The GPU, labelled like a cluster (its name over its figure), one
        // tank for the device (Apple GPUs report device-wide use only).
        let start = x;
        let m = Mark::of(&gpu.load, 1.0, words::LOAD_GATE);
        p.label(x, name_base, "GPU".into(), inks.words, true, true);
        p.figure(
            x,
            count_base,
            format!("{:.0}%", (gpu.load.raw * 100.0).round()),
            inks.words2,
            true,
        );
        x += gpu_lw;
        let tw = (tile * 0.62).round().max(p.px(6.0));
        p.tank((x, top, tw, block_h), m, gpu.cores, gap * 2.0);
        x += tw + gap;
        hits.push(Hit {
            rect: (start, top, x - start, block_h),
            instrument: Instrument::Gpu,
            bar: Bar::Global,
        });
    }
    x
}

/// Placeholder digits for a figure's widest form: `8` per digit of `n`.
fn eights(n: usize) -> String {
    "8".repeat(n.max(1).to_string().len())
}

/// The three rows' contents.
fn rows<'a>(snap: &'a Snapshot, input: &DeckInput<'_>, inks: &VitalsInks) -> Vec<Row<'a>> {
    let total_cores = snap.total_cores().map_or(1.0, |n| n as f32).max(1.0);
    let tab: Option<&TabView> = input.tab_key.and_then(|k| snap.tab(k));
    let cpu_motion = |t: &TrendView| t.motion(words::CORES_GATE);
    let mut rows: Vec<Row<'_>> = Vec::with_capacity(3);
    {
        let busy = snap.cpu.as_ref().map(|c| c.busy);
        let cpu_fig = busy.map_or_else(
            || "\u{2014}".to_string(),
            |b| words::cores_figure(b.level, snap.total_cores(), cpu_motion(&b)),
        );
        let mut clauses = Vec::new();
        let mem_fig = snap.mem.as_ref().map_or_else(
            || "\u{2014}".to_string(),
            |m| {
                let mut f = format!(
                    "{}/{}G",
                    words::gib_figure(m.sample.used()),
                    words::gib_total(m.sample.total)
                );
                if m.sample.swap_used > 0 {
                    f.push_str(&format!(" swap {}", words::mem(m.sample.swap_used)));
                }
                f
            },
        );
        if let Some(m) = snap.mem.as_ref() {
            // Windows fails an allocation at its commit limit while RAM is
            // still free (design §9.6): near the limit the bar says so, in
            // the warn ink, ahead of the pressure it raises, so it is the
            // clause that stays when the words fold. Linux's heuristic
            // commit is never said.
            if let Some(pct) = m.sample.commit.and_then(|c| c.near_limit()) {
                clauses.push(
                    Clause::new(format!("commit {pct}% of limit"), inks.warn)
                        .or_short(format!("commit {pct}%")),
                );
            }
            match m.sample.pressure {
                aterm_vitals::Pressure::Warn => clauses
                    .push(Clause::new("memory pressure warn", inks.warn).or_short("pressure warn")),
                aterm_vitals::Pressure::Critical => clauses.push(
                    Clause::new("memory pressure critical", inks.critical)
                        .or_short("pressure critical"),
                ),
                _ => {}
            }
        }
        if let Some(v) = snap.volume.as_ref() {
            let low = v.total > 0 && (v.available as f64) < v.total as f64 * 0.1;
            let free = words::disk(v.available);
            let short = free
                .replace(" TB", "T")
                .replace(" GB", "G")
                .replace(" MB", "M");
            let mark = if low { "\u{25E2} " } else { "" };
            clauses.push(
                Clause::new(
                    format!("{mark}disk {free} free"),
                    if low { inks.warn } else { inks.words2 },
                )
                .or_short(format!("{mark}{short} free")),
            );
        }
        rows.push(Row {
            bar: Bar::Global,
            label: "Global".into(),
            scope: Some(&snap.global),
            cpu_ink: busy.map_or(inks.load[0], |b| inks.load_at(b.level / total_cores)),
            mem_ink: inks.mem[0],
            cpu_fig,
            mem_fig,
            clauses,
        });
    }
    {
        let at = snap.aterm.as_ref();
        let cpu_fig = at.map_or_else(
            || "\u{2014}".to_string(),
            |sc| words::cores_figure(sc.cores.level, None, cpu_motion(&sc.cores)),
        );
        let mem_fig = at
            .and_then(|sc| sc.footprint)
            .map_or_else(String::new, |f| {
                let mut s = words::mem(f);
                if let Some(g) = at
                    .map(|sc| sc.footprint_gib.motion(words::FOOTPRINT_GATE_GIB))
                    .and_then(Motion::glyph)
                {
                    s.push(g);
                }
                s
            });
        let mut clauses = Vec::new();
        match at {
            None => clauses.push(Clause::new("measuring", inks.words2)),
            Some(sc) => {
                // Its share of what is busy, always with its noun: never a
                // bare percentage.
                if let Some(b) = snap
                    .cpu
                    .as_ref()
                    .map(|c| c.busy.level)
                    .filter(|b| *b > 0.05)
                {
                    clauses.push(Clause::new(
                        format!(
                            "{:.0}% of busy",
                            (sc.cores.level / b * 100.0).clamp(0.0, 100.0)
                        ),
                        inks.words2,
                    ));
                }
                if let Some((name, c)) = sc.top.as_ref().filter(|(_, c)| *c >= 0.05) {
                    clauses.push(Clause::new(
                        format!("{name} {}c", words::cores(*c)),
                        inks.words2,
                    ));
                }
                clauses.push(Clause::new(format!("{} proc", sc.procs), inks.words2));
            }
        }
        rows.push(Row {
            bar: Bar::Aterm,
            label: "aterm".into(),
            scope: at,
            cpu_ink: inks.aterm,
            mem_ink: inks.aterm,
            cpu_fig,
            mem_fig,
            clauses,
        });
    }
    {
        let sc = tab.map(|t| &t.scope);
        let label = tab.map_or_else(|| input.tab_label.clone(), |t| t.label.clone());
        let cpu_fig = sc.map_or_else(
            || "\u{2014}".to_string(),
            |sc| words::cores_figure(sc.cores.level, None, cpu_motion(&sc.cores)),
        );
        let mem_fig = sc
            .and_then(|sc| sc.footprint)
            .map_or_else(String::new, words::mem);
        let mut clauses = Vec::new();
        match tab {
            None => clauses.push(Clause::new("measuring", inks.words2)),
            Some(t) => {
                if !t.program.is_empty() {
                    clauses.push(Clause::new(t.program.clone(), inks.words));
                }
                // With one tab the row says so (§12.2), before the clauses
                // that fold first, so every width that shows the program
                // shows it too.
                if input.tab_count == 1 {
                    clauses.push(Clause::new("the only tab", inks.words2));
                }
                if let Some((name, c)) = t
                    .scope
                    .top
                    .as_ref()
                    .filter(|(n, c)| *c >= 0.05 && *n != t.program)
                {
                    clauses.push(Clause::new(
                        format!("{name} {}c", words::cores(*c)),
                        inks.words2,
                    ));
                }
                clauses.push(Clause::new(format!("{} proc", t.scope.procs), inks.words2));
            }
        }
        rows.push(Row {
            bar: Bar::Tab,
            label,
            scope: sc,
            cpu_ink: inks.tab,
            mem_ink: inks.tab,
            cpu_fig,
            mem_fig,
            clauses,
        });
    }
    rows
}

/// [`deck_prims`] with the hit regions a pointer can rest on (card pixels).
pub(crate) fn deck_paint(g: &DeckGeom, input: &DeckInput<'_>) -> (Vec<DrawPrim>, Vec<Hit>) {
    let inks = input.inks;
    let s = g.scale.max(0.5);
    let mut p = Painter::new(&inks, s, g.font_px, input.dwc);
    let mut hits = Vec::new();
    let (w, h) = (g.w.round(), g.h.round());
    // The ground, and a hairline seam to the grid below.
    p.prims
        .push(panel(0.0, 0.0, w, h, 0.0, rgba(inks.ground, 0xFF)));
    let seam = p.hair();
    p.prims
        .push(panel(0.0, h - seam, w, seam, 0.0, rgba(input.seam, 0xFF)));

    let margin_y = p.px(4.0);
    let left = g.pad.round().max(p.px(8.0));
    let right = w - left;
    let Some(snap) = input.snap else {
        let base = crate::tray_raster::row_baseline(0.0, h, p.text_px.get());
        p.label(
            left,
            base,
            "Reading the machine\u{2026}".into(),
            inks.words2,
            false,
            false,
        );
        return (p.prims, hits);
    };
    let inner_h = h - 2.0 * margin_y - seam;
    let mut x = left;

    if let Some(cpu) = snap.cpu.as_ref() {
        x = core_block(
            &mut p,
            cpu,
            snap.gpu.as_ref(),
            x,
            margin_y,
            inner_h,
            (right - left) * 0.32,
            &mut hits,
        );
        x += p.px(6.0);
    }

    let total_cores = snap.total_cores().map_or(1.0, |n| n as f32).max(1.0);
    let mem_total = snap.mem.as_ref().map_or(0, |m| m.sample.total);
    let rows = rows(snap, input, &inks);

    let row_gap = p.px(3.0);
    let row_h = ((inner_h - 2.0 * row_gap) / 3.0).floor();
    let lane_h = (row_h * 0.40).round().max(p.px(2.0));
    let gap = p.px(5.0);
    let key_w = p.px(2.5).max(2.0);
    let key_h = (p.text_px.get() * 0.70).round();
    let label_w = rows
        .iter()
        .map(|r| p.label_w(&r.label, true, false))
        .fold(0.0f32, f32::max)
        + key_w
        + p.px(4.0)
        + gap;
    let n = snap.total_cores().unwrap_or(8);
    let cpu_fig_w = p.figure_w(&format!("{}.8c/{}\u{25B4}", eights(n), eights(n)), false) + gap;
    // The memory figure's column holds the widest figure this machine can
    // show now: swap widens it only while swap is in use.
    let swapping = snap.mem.as_ref().is_some_and(|m| m.sample.swap_used > 0);
    let total_word = snap
        .mem
        .as_ref()
        .map_or_else(|| "32".to_string(), |m| words::gib_total(m.sample.total));
    let mem_fig_w = if swapping {
        p.figure_w(&format!("88.8/{total_word}G swap 88.8 GiB"), false)
    } else {
        p.figure_w(&format!("88.8/{total_word}G"), false)
            .max(p.figure_w("888 MiB\u{25B4}", false))
    } + gap;
    let bracket_w = p.px(3.0);
    let room = (right - x - label_w - cpu_fig_w - mem_fig_w - bracket_w).max(0.0);
    // The words keep a column of their own, a fixed share of the room, so
    // the lanes never move when a name changes; they leave only when the
    // room is too small for the shortest clause.
    let words_w = if room > p.px(150.0) {
        (room * 0.30)
            .max(p.label_w("disk 8.88 TB free", false, false))
            .min(p.label_w(&"m".repeat(40), false, false))
            .floor()
    } else {
        0.0
    };
    let avail = room - words_w;
    // Width shares of the rest: the CPU lane, the sparkline, the memory lane.
    // The trace stays down to a half-screen window (110 columns at 2×): it
    // is the bar's two minutes, and the lanes give up width before it does.
    let (cpu_w, spark_w, mem_w) = if avail > p.px(200.0) {
        let spark = (avail * 0.18).floor().clamp(p.px(40.0), p.px(120.0));
        let rest = avail - spark - gap;
        ((rest * 0.55).floor(), spark, (rest * 0.45).floor())
    } else if avail > p.px(90.0) {
        ((avail * 0.56).floor(), 0.0, (avail * 0.44 - gap).floor())
    } else {
        (avail.floor(), 0.0, 0.0)
    };
    let pinned = snap
        .cpu
        .as_ref()
        .is_some_and(|c| c.busy.level >= total_cores * 0.98);
    for (i, row) in rows.iter().enumerate() {
        let ry = margin_y + i as f32 * (row_h + row_gap);
        let base = crate::tray_raster::row_baseline(ry, row_h, p.text_px.get());
        let ly = (ry + (row_h - lane_h) / 2.0).round();
        let mut cx = x;
        if i > 0 {
            // An engraved groove between two bars.
            let gy = (ry - row_gap / 2.0).floor();
            p.prims.push(panel(
                x,
                gy,
                right - x,
                p.hair(),
                0.0,
                rgba(inks.groove, 0xFF),
            ));
        }
        // The key swatch, then the label: the swatch is the bar's legend.
        let ky = (ry + (row_h - key_h) / 2.0).round();
        match row.bar {
            Bar::Global => p.ramp_keyline(cx, ky, key_w, key_h),
            Bar::Aterm => p.keyline(cx, ky, key_w, key_h, inks.aterm),
            Bar::Tab => p.keyline(cx, ky, key_w, key_h, inks.tab),
        }
        p.label(
            cx + key_w + p.px(4.0),
            base,
            row.label.clone(),
            inks.words,
            true,
            false,
        );
        let row_instrument_end = cx + label_w;
        hits.push(Hit {
            rect: (cx, ry, label_w, row_h),
            instrument: Instrument::Cores,
            bar: row.bar,
        });
        cx = row_instrument_end;
        let slot = (
            cx - gap / 2.0,
            ry - row_gap / 2.0,
            cpu_w + gap,
            row_h + row_gap,
        );
        if cpu_w > 0.0 {
            match row.scope {
                Some(sc) => {
                    let m = Mark::of(&sc.cores, total_cores, words::CORES_GATE);
                    p.lane(cx, ly, cpu_w, lane_h, m, row.cpu_ink, slot, false);
                }
                None => p.prims.push(panel(
                    cx,
                    ly,
                    cpu_w,
                    lane_h,
                    (lane_h / 2.0).floor(),
                    a(inks.track, 0.6),
                )),
            }
            let bh = lane_h + p.px(4.0);
            p.bracket(
                cx + cpu_w + p.px(1.0),
                (ry + (row_h - bh) / 2.0).round(),
                bh,
                pinned && row.bar == Bar::Global,
            );
            hits.push(Hit {
                rect: (cx, ry, cpu_w + bracket_w, row_h),
                instrument: Instrument::Cores,
                bar: row.bar,
            });
            cx += cpu_w + bracket_w;
        }
        p.figure(cx, base, row.cpu_fig.clone(), inks.words, false);
        cx += cpu_fig_w;
        if spark_w > 0.0 {
            if let Some(sc) = row.scope {
                let vals: Vec<Option<f32>> = sc
                    .history
                    .iter()
                    .map(|v| v.map(|c| c / total_cores))
                    .collect();
                p.spark(
                    cx,
                    (ry + row_h * 0.12).round(),
                    spark_w,
                    (row_h * 0.76).round(),
                    &vals,
                    row.cpu_ink,
                );
                hits.push(Hit {
                    rect: (cx, ry, spark_w, row_h),
                    instrument: Instrument::Cores,
                    bar: row.bar,
                });
            }
            cx += spark_w + gap;
        }
        if mem_w > 0.0 {
            let mem_slot = (
                cx - gap / 2.0,
                ry - row_gap / 2.0,
                mem_w + gap,
                row_h + row_gap,
            );
            if row.bar == Bar::Global {
                if let Some(m) = snap.mem.as_ref() {
                    global_memory(&mut p, cx, ly, mem_w, lane_h, m);
                }
            } else if let (Some(sc), true) = (row.scope, mem_total > 0) {
                let mut t = sc.footprint_gib;
                let cap = aterm_vitals::model::gib(mem_total);
                if t.level == 0.0 && sc.footprint.is_some() {
                    t = TrendView::flat(aterm_vitals::model::gib(sc.footprint.unwrap_or(0)));
                }
                let m = Mark::of(&t, cap, words::FOOTPRINT_GATE_GIB);
                p.lane(cx, ly, mem_w, lane_h, m, row.mem_ink, mem_slot, false);
            }
            hits.push(Hit {
                rect: (cx, ry, mem_w + gap, row_h),
                instrument: Instrument::Memory,
                bar: row.bar,
            });
            cx += mem_w + gap / 2.0;
        }
        if !row.mem_fig.is_empty() {
            p.figure(cx, base, row.mem_fig.clone(), inks.words, false);
        }
        cx += mem_fig_w;
        if words_w > 0.0 || cx < right {
            let mut wx = cx;
            if row.bar == Bar::Aterm && !input.marks.is_empty() {
                // The attention marks lead the aterm bar's words.
                let r = (p.text_px.get() * 0.36).round();
                for m in input.marks.iter().take(3) {
                    p.mark(m.kind, wx + r, (ry + row_h / 2.0).round(), r, m.frac);
                    wx += 2.0 * r + p.px(2.0);
                    let c = m.count.to_string();
                    let cw = p.figure_w(&c, false);
                    let ink = if m.kind == WellMark::Stuck {
                        inks.stuck
                    } else {
                        inks.words2
                    };
                    p.figure(wx, base, c, ink, false);
                    wx += cw + p.px(6.0);
                }
                hits.push(Hit {
                    rect: (cx, ry, wx - cx, row_h),
                    instrument: Instrument::Jobs,
                    bar: Bar::Aterm,
                });
            }
            let parts = p.fit_clauses(&row.clauses, (right - wx).max(0.0), false);
            if !parts.is_empty() {
                let end = p.clauses(wx, base, parts, false);
                if row.bar == Bar::Global && snap.volume.is_some() {
                    hits.push(Hit {
                        rect: (wx, ry, end - wx, row_h),
                        instrument: Instrument::Disk,
                        bar: Bar::Global,
                    });
                }
            }
        }
    }
    (p.prims, hits)
}

/// The machine's memory by class in one lane: used, compressed, cached
/// files, then free as the track, with the pressure cap at the end when
/// pressure is not normal.
pub(crate) fn global_memory(
    p: &mut Painter<'_>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    m: &aterm_vitals::model::MemView,
) {
    let inks = *p.inks;
    let total = m.sample.total.max(1) as f32;
    let r = (h / 2.0).floor();
    p.prims.push(panel(x, y, w, h, r, rgba(inks.track, 0xFF)));
    p.prims.push(DrawPrim::ClipPush { x, y, w, h });
    let used_plain = m.sample.app.saturating_add(m.sample.wired) as f32;
    let parts = [
        (used_plain, inks.mem[0]),
        (m.sample.compressed.unwrap_or(0) as f32, inks.mem[1]),
        (m.sample.cached as f32, inks.mem[2]),
    ];
    let mut mx = x;
    for (k, (bytes, c)) in parts.iter().enumerate() {
        let pw = (bytes / total * w).round().min(x + w - mx);
        if pw >= 1.0 {
            let (px0, pw0) = if k == 0 { (mx - r, pw + r) } else { (mx, pw) };
            p.prims.push(panel(
                px0,
                y,
                pw0,
                h,
                if k == 0 { r } else { 0.0 },
                rgba(*c, 0xFF),
            ));
            mx += pw;
        }
    }
    p.prims.push(DrawPrim::ClipPop);
    let cap = match m.sample.pressure {
        aterm_vitals::Pressure::Warn => Some(inks.warn),
        aterm_vitals::Pressure::Critical => Some(inks.critical),
        _ => None,
    };
    if let Some(c) = cap {
        let cw = p.px(2.0).max(2.0);
        p.prims.push(panel(
            (x + (m.sample.used() as f32 / total * w).round() - cw).max(x),
            y - 1.0,
            cw,
            h + 2.0,
            0.0,
            rgba(c, 0xFF),
        ));
    }
}

/// A fingerprint of the prims, so an identical layout presents nothing.
/// Every position is a whole device pixel, so the Debug text is stable.
pub(crate) fn prims_fp(prims: &[DrawPrim]) -> u64 {
    let text = format!("{prims:?}");
    aterm_hash::fnv1a_64(text.as_bytes()) | 1
}

/// The stats card's inputs for a window, read off the App: which form, the
/// card geometry and placement, the tab, the inks. `None` when the window
/// shows no bars.
pub(crate) struct DeckFrame {
    pub(crate) form: StatsBars,
    /// The sill's plate alpha ([`DeckInput::plate`]).
    pub(crate) plate: Option<u8>,
    pub(crate) geom: DeckGeom,
    pub(crate) dy: u32,
    pub(crate) tab_key: Option<u64>,
    pub(crate) tab_label: String,
    pub(crate) tab_count: usize,
    pub(crate) inks: VitalsInks,
    /// The lens's inks: always solved on the band ground (the lens sits in
    /// band-ground wells over either form).
    pub(crate) lens_inks: VitalsInks,
    pub(crate) seam: Rgb,
    pub(crate) dwc: bool,
}

impl crate::App {
    /// The window's frame height as the presenters compose it (the head, the
    /// top pad, the rows and the strip, the bottom pad), in device px.
    pub(crate) fn stats_frame_h(&self, wid: crate::WindowId) -> usize {
        let Some(ws) = self.windows.get(&wid) else {
            return 0;
        };
        let (_, ch) = self.win_cell_size(wid);
        let composed_rows = usize::from(ws.rows).saturating_add(usize::from(self.chrome_rows(wid)));
        composed_rows
            .saturating_mul(ch)
            .saturating_add(self.win_head(wid))
            .saturating_add(self.win_pad_top(wid))
            .saturating_add(self.win_pad(wid))
    }

    pub(crate) fn deck_frame(&self, wid: crate::WindowId) -> Option<DeckFrame> {
        let form = self.stats_bars_form();
        // Off pays nothing: this runs on every frame's fingerprint and every
        // pointer event.
        if form == StatsBars::Off {
            return None;
        }
        let ws = self.windows.get(&wid)?;
        if !stats_drawn_on(ws) {
            return None;
        }
        if ws.cols == 0 {
            return None;
        }
        let (cw, _) = self.win_cell_size(wid);
        let pad = self.win_pad(wid);
        let w = ws.cols as usize * cw + 2 * pad;
        let (tab_key, tab_label) = self.deck_tab(wid);
        let tab_count = ws.tab_set.tabs().len();
        let dwc = crate::native_appearance::current_preferences().differentiate_without_color;
        let palette = self.chrome_palette(crate::app_render::ChromeSurface::Header);
        let band = crate::chrome_band::band_colors(palette);
        let band_inks = cached_inks(band.bar_bg, band.value, band.accent);
        match form {
            StatsBars::Off => None,
            StatsBars::Deck => {
                let rows = self.deck_rows_px(wid)?;
                let (dy, deck) = (rows.start, rows.len());
                Some(DeckFrame {
                    form,
                    plate: None,
                    geom: DeckGeom {
                        w: w as f32,
                        h: deck as f32,
                        pad: pad as f32,
                        scale: ws.scale as f32,
                        font_px: self.win_font_px(wid),
                    },
                    dy: u32::try_from(dy).unwrap_or(0),
                    tab_key,
                    tab_label,
                    tab_count,
                    inks: band_inks,
                    lens_inks: band_inks,
                    seam: band.seam,
                    dwc,
                })
            }
            StatsBars::Sill => {
                let sill = crate::stats_sill::sill_px(pad, ws.scale);
                if sill == 0 {
                    return None;
                }
                let frame_h = self.stats_frame_h(wid);
                // The sill's plate is the terminal's own floor: its inks are
                // solved on the background the pad is painted in (§12.2
                // "Plate"), the front tab's own theme when it draws one.
                let (floor, value, accent) = self.sill_floor(tab_key);
                let prefs = crate::native_appearance::current_preferences();
                let translucent = self.config.background_opacity_or_default() < 1.0
                    || self.config.wallpaper.is_some();
                let plate = translucent.then_some(if prefs.reduced_transparency {
                    0xFF
                } else {
                    235
                });
                Some(DeckFrame {
                    form,
                    plate,
                    geom: DeckGeom {
                        w: w as f32,
                        h: sill as f32,
                        pad: pad as f32,
                        scale: ws.scale as f32,
                        font_px: self.win_font_px(wid),
                    },
                    dy: u32::try_from(frame_h.saturating_sub(pad)).unwrap_or(0),
                    tab_key,
                    tab_label,
                    tab_count,
                    inks: cached_inks(floor, value, accent),
                    lens_inks: band_inks,
                    seam: floor,
                    dwc,
                })
            }
        }
    }

    /// The sill's floor, words and accent: the front tab's session's own
    /// theme when it draws one (Choose Tab Theme, `theme session set`), else
    /// the instance's.
    fn sill_floor(&self, tab_key: Option<u64>) -> (Rgb, Rgb, Rgb) {
        let rgb = |c: u32| [(c >> 16) as u8, (c >> 8) as u8, c as u8];
        let theme = self.theme;
        match tab_key.and_then(|sid| self.pool.own_colors(sid)) {
            Some(own) => {
                let c = |v: aterm_types::Rgb| [v.r, v.g, v.b];
                (
                    c(own.default_background),
                    c(own.default_foreground),
                    own.cursor_color.map_or(rgb(theme.cursor), c),
                )
            }
            None => (rgb(theme.bg), rgb(theme.fg), rgb(theme.cursor)),
        }
    }

    /// The instance's attention marks for the bars, worst first, from the
    /// stuck engine's view on the snapshot (design §11): empty while nothing
    /// is stuck, quiet, waiting, parked or unreaped, and before the first
    /// jobs pass.
    pub(crate) fn stats_marks(&self) -> Vec<StatsMark> {
        self.vitals
            .snapshot
            .as_ref()
            .and_then(|s| s.jobs.as_deref())
            .map(crate::stats_paint::marks_of)
            .unwrap_or_default()
    }

    /// The pointer in the stats card's own pixels, while it is over the card.
    pub(crate) fn stats_card_point(&self, wid: crate::WindowId) -> Option<(f64, f64)> {
        let f = self.deck_frame(wid)?;
        let (x, y) = self.windows.get(&wid).map(|ws| ws.last_cursor_px)?;
        let (ox, oy) = self.frame_origin(wid);
        let (fx, fy) = (x - ox as f64, y - oy as f64);
        let top = f64::from(f.dy);
        ((top..top + f64::from(f.geom.h)).contains(&fy) && (0.0..f64::from(f.geom.w)).contains(&fx))
            .then_some((fx, fy - top))
    }

    /// The instrument under the pointer on the stats card, from the hit
    /// regions its last layout recorded.
    pub(crate) fn stats_hit_under_pointer(&self, wid: crate::WindowId) -> Option<Hit> {
        let (x, y) = self.stats_card_point(wid)?;
        let _ = self.stats_deck_fp(wid);
        let ws = self.windows.get(&wid)?;
        let hits = ws.stats_hits.borrow();
        hits.iter()
            .find(|h| crate::stats_paint::contains(h.rect, x, y))
            .copied()
    }

    /// A cheap hash of everything the card's prims read: the snapshot's
    /// sequence, the form, the geometry and placement, the tab and the inks.
    fn deck_input_key(&self, f: &DeckFrame) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.vitals.snapshot.as_ref().map(|s| s.seq).hash(&mut h);
        f.form.hash(&mut h);
        for v in [f.geom.w, f.geom.h, f.geom.pad, f.geom.scale, f.geom.font_px] {
            v.to_bits().hash(&mut h);
        }
        f.dy.hash(&mut h);
        f.tab_key.hash(&mut h);
        f.tab_label.hash(&mut h);
        f.tab_count.hash(&mut h);
        f.dwc.hash(&mut h);
        f.plate.hash(&mut h);
        format!("{:?}", f.inks).hash(&mut h);
        f.seam.hash(&mut h);
        // The marks come from the snapshot, whose sequence is hashed above.
        h.finish() | 1
    }

    /// The card's prims, laid out only when their inputs moved
    /// (`WindowState::deck_memo` keeps the last input key and prims' print;
    /// `WindowState::stats_hits` the hit regions).
    fn deck_layout(&self, wid: crate::WindowId, f: &DeckFrame, key: u64) -> Vec<DrawPrim> {
        let input = DeckInput {
            snap: self.vitals.snapshot.as_deref(),
            tab_key: f.tab_key,
            tab_label: f.tab_label.clone(),
            tab_count: f.tab_count,
            inks: f.inks,
            seam: f.seam,
            dwc: f.dwc,
            marks: self.stats_marks(),
            plate: f.plate,
        };
        let (prims, hits) = match f.form {
            StatsBars::Sill => crate::stats_sill::sill_paint(&f.geom, &input),
            _ => deck_paint(&f.geom, &input),
        };
        if let Some(ws) = self.windows.get(&wid) {
            ws.deck_memo
                .set((key, prims_fp(&prims) ^ u64::from(f.dy).rotate_left(40)));
            *ws.stats_hits.borrow_mut() = hits;
        }
        prims
    }

    /// The stats card's fingerprint for `wid` (`0` when it shows none), the
    /// `RepaintKey` term: whole-pixel prims, so a new snapshot that moves no
    /// drawn pixel presents nothing. The open lens folds in.
    pub(crate) fn stats_deck_fp(&self, wid: crate::WindowId) -> u64 {
        let lens = self.stats_lens_fp(wid);
        let Some(f) = self.deck_frame(wid) else {
            return lens;
        };
        let key = self.deck_input_key(&f);
        if let Some((k, fp)) = self.windows.get(&wid).map(|ws| ws.deck_memo.get())
            && k == key
        {
            return fp ^ lens;
        }
        let _ = self.deck_layout(wid, &f, key);
        self.windows.get(&wid).map_or(0, |ws| ws.deck_memo.get().1) ^ lens
    }

    /// Rasterize the stats card into `wid`'s paint-only `deck_card`,
    /// re-rasterizing only when its prims or placement moved, and the lens
    /// into its own card. `None` when it shows none.
    pub(crate) fn splice_stats_deck(&mut self, wid: crate::WindowId) {
        self.splice_stats_lens(wid);
        let Some(f) = self.deck_frame(wid) else {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.deck_card = None;
                ws.deck_fold = None;
                ws.stats_hits.borrow_mut().clear();
            }
            return;
        };
        let key = self.deck_input_key(&f);
        if self
            .windows
            .get(&wid)
            .and_then(|ws| ws.deck_card.as_ref())
            .is_some_and(|c| c.geom == key)
        {
            return;
        }
        let prims = self.deck_layout(wid, &f, key);
        let fp = self.windows.get(&wid).map_or(0, |ws| ws.deck_memo.get().1);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        if let Some(card) = ws.deck_card.as_mut()
            && card.fp == fp
        {
            // New inputs, the same pixels: keep the raster.
            card.geom = key;
            return;
        }
        let (rgba, pw, ph) = crate::tray_raster::rasterize_tray(
            &prims,
            f.geom.w as u32,
            f.geom.h as u32,
            1.0,
            [0, 0, 0, 0],
        );
        ws.deck_card = Some(crate::SettingsCard {
            scrim: None,
            rgba,
            pw,
            ph,
            dx: 0,
            dy: f.dy,
            fp,
            geom: key,
            raster_fingerprint_once: std::sync::OnceLock::new(),
        });
    }

    /// Fold the stats card and the open lens under the card the window would
    /// otherwise present (the backends take one tray surface), cached by the
    /// cards' identities. Runs after every other card's splice in each paint
    /// path.
    pub(crate) fn fold_stats_deck(&mut self, wid: crate::WindowId) {
        use std::hash::{Hash, Hasher};
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let top = ws
            .route_card
            .as_ref()
            .or(ws.settings_card.as_ref())
            .or(ws.badge_card.as_ref());
        if top.is_some_and(|t| t.scrim.is_some()) {
            // A modal with a scrim covers the window, the bars with it.
            ws.deck_fold = None;
            return;
        }
        let cards: Vec<&crate::SettingsCard> = [ws.deck_card.as_ref(), ws.lens_card.as_ref(), top]
            .into_iter()
            .flatten()
            .collect();
        // The stats card alone, or another card alone, needs no fold: the
        // presenters take it as it is (`WindowState::present_card`). An open
        // lens always folds, since no presenter takes the lens card itself.
        if ws.lens_card.is_none() && (cards.len() < 2 || ws.deck_card.is_none()) {
            ws.deck_fold = None;
            return;
        }
        let mut identity = std::collections::hash_map::DefaultHasher::new();
        for card in &cards {
            card.fp.hash(&mut identity);
            card.geom.hash(&mut identity);
            card.dx.hash(&mut identity);
            card.dy.hash(&mut identity);
            card.pw.hash(&mut identity);
            card.ph.hash(&mut identity);
        }
        let fp = identity.finish() | 1;
        if ws.deck_fold.as_ref().is_some_and(|c| c.fp == fp) {
            return;
        }
        let x0 = cards.iter().map(|c| c.dx).min().unwrap_or(0);
        let y0 = cards.iter().map(|c| c.dy).min().unwrap_or(0);
        let x1 = cards.iter().map(|c| c.dx + c.pw).max().unwrap_or(0);
        let y1 = cards.iter().map(|c| c.dy + c.ph).max().unwrap_or(0);
        let (pw, ph) = (x1 - x0, y1 - y0);
        let Some(len) = usize::try_from(u64::from(pw) * u64::from(ph) * 4).ok() else {
            ws.deck_fold = None;
            return;
        };
        let mut rgba = vec![0; len];
        let ok = cards.iter().all(|card| {
            crate::tray_raster::composite_rgba_surface(
                &mut rgba,
                (pw, ph),
                (x0, y0),
                &card.rgba,
                (card.pw, card.ph),
                (card.dx, card.dy),
            )
        });
        ws.deck_fold = ok.then_some(crate::SettingsCard {
            scrim: None,
            rgba,
            pw,
            ph,
            dx: x0,
            dy: y0,
            fp,
            geom: fp,
            raster_fingerprint_once: std::sync::OnceLock::new(),
        });
    }
}

/// The stats bars as a reader meets them (design §12.2 Accessibility): a
/// group a bar named for its scope and described by its sentence, its meters
/// with value and range, the core row a grid of per-core meters. Pure, so a
/// test reads it without an App.
#[cfg(a11y_tree)]
pub(crate) fn stats_a11y_groups(
    snap: &Snapshot,
    tab_key: Option<u64>,
    tab_label: &str,
    marks: &[StatsMark],
) -> Vec<crate::a11y_tree::StatsGroup> {
    use crate::a11y_tree::{StatsGroup, StatsMeter};
    let gib = aterm_vitals::model::gib;
    let total_cores = snap.total_cores().unwrap_or(0);
    let mem_total = snap.mem.as_ref().map_or(0, |m| m.sample.total);
    let motion = |t: &TrendView, gate: f32| {
        t.motion(gate)
            .word()
            .map_or(String::new(), |w| format!(", {w}"))
    };
    let cpu_meter = |level: f32, words: String| StatsMeter {
        name: "CPU".into(),
        value: f64::from(level),
        min: 0.0,
        max: total_cores.max(1) as f64,
        words,
    };
    let footprint_meter = |bytes: u64| StatsMeter {
        name: "Memory".into(),
        value: f64::from(gib(bytes)),
        min: 0.0,
        max: f64::from(gib(mem_total.max(1))),
        words: format!("{} footprint", words::mem(bytes)),
    };
    let mut global = Vec::new();
    if let Some(cpu) = snap.cpu.as_ref() {
        global.push(cpu_meter(
            cpu.busy.level,
            format!(
                "{} of {} cores busy{}",
                words::cores(cpu.busy.level),
                cpu.total(),
                motion(&cpu.busy, words::CORES_GATE)
            ),
        ));
    }
    if let Some(g) = snap.gpu.as_ref() {
        let pct = (g.load.raw * 100.0).round();
        global.push(StatsMeter {
            name: "GPU".into(),
            value: f64::from(pct),
            min: 0.0,
            max: 100.0,
            words: format!("{pct:.0} percent"),
        });
    }
    if let Some(m) = snap.mem.as_ref() {
        global.push(StatsMeter {
            name: "Memory".into(),
            value: f64::from(gib(m.sample.used())),
            min: 0.0,
            max: f64::from(gib(m.sample.total)),
            words: format!(
                "{} of {} GiB used, pressure {}",
                words::gib_figure(m.sample.used()),
                words::gib_total(m.sample.total),
                m.sample.pressure.word()
            ),
        });
    }
    if let Some(v) = snap.volume.as_ref() {
        let used = v.total.saturating_sub(v.available);
        global.push(StatsMeter {
            name: "Disk".into(),
            value: used as f64 / 1e9,
            min: 0.0,
            max: v.total as f64 / 1e9,
            words: format!(
                "{} free of {}",
                words::disk(v.available),
                words::disk(v.total)
            ),
        });
    }
    let cores = snap.cpu.as_ref().map_or_else(Vec::new, |cpu| {
        cpu.clusters
            .iter()
            .flat_map(|cluster| {
                cluster.cores.iter().enumerate().filter_map(|(k, &core)| {
                    let view = cpu.cores.get(core)?;
                    let m = Mark::of(&view.load, 1.0, words::LOAD_GATE);
                    let pct = (view.load.level * 100.0).round();
                    Some(StatsMeter {
                        name: format!("{} core {}", cluster.name, k + 1),
                        value: f64::from(pct),
                        min: 0.0,
                        max: 100.0,
                        words: format!(
                            "{pct:.0} %{}, {}",
                            motion(&view.load, words::LOAD_GATE),
                            m.edge_word()
                        ),
                    })
                })
            })
            .collect()
    });
    let mut aterm = Vec::new();
    if let Some(a) = snap.aterm.as_ref() {
        aterm.push(cpu_meter(
            a.cores.level,
            format!(
                "{} cores{}",
                words::cores(a.cores.level),
                motion(&a.cores, words::CORES_GATE)
            ),
        ));
        if let Some(f) = a.footprint {
            aterm.push(footprint_meter(f));
        }
    }
    let mut tab_meters = Vec::new();
    let tab = tab_key.and_then(|k| snap.tab(k));
    if let Some(t) = tab {
        tab_meters.push(cpu_meter(
            t.scope.cores.level,
            format!(
                "{} cores{}",
                words::cores(t.scope.cores.level),
                motion(&t.scope.cores, words::CORES_GATE)
            ),
        ));
        if let Some(f) = t.scope.footprint {
            tab_meters.push(footprint_meter(f));
        }
    }
    let marks = (!marks.is_empty()).then(|| {
        marks
            .iter()
            .map(|m| m.words())
            .collect::<Vec<_>>()
            .join(", ")
    });
    vec![
        StatsGroup {
            name: "Global",
            sentence: words::global_sentence(snap),
            meters: global,
            cores,
            marks: None,
        },
        StatsGroup {
            name: "aterm",
            sentence: words::aterm_sentence(snap),
            meters: aterm,
            cores: Vec::new(),
            marks,
        },
        StatsGroup {
            name: "Tab",
            sentence: crate::stats_lens::sentence(snap, Bar::Tab, tab_key, tab_label),
            meters: tab_meters,
            cores: Vec::new(),
            marks: None,
        },
    ]
}

impl crate::App {
    /// The stats bars for `wid`'s accessibility tree, while it shows them.
    #[cfg(a11y_tree)]
    pub(crate) fn stats_a11y(&self, wid: crate::WindowId) -> Option<crate::a11y_tree::GridStats> {
        let f = self.deck_frame(wid)?;
        let snap = self.vitals.snapshot.as_deref()?;
        let (ox, oy) = self.frame_origin(wid);
        let bounds = aterm_a11y::model::Rect {
            x0: ox as f64,
            y0: oy as f64 + f64::from(f.dy),
            x1: ox as f64 + f64::from(f.geom.w),
            y1: oy as f64 + f64::from(f.dy) + f64::from(f.geom.h),
        };
        let lens = self.lens_frame(wid).map(|(lg, lens, _)| {
            let bar = match lens.bar {
                Bar::Global => "Global",
                Bar::Aterm => "aterm",
                Bar::Tab => "Tab",
            };
            crate::a11y_tree::StatsLens {
                keyboard: lens.how == crate::stats_lens::Opened::Keyboard,
                bar,
                bar_words: crate::stats_lens::sentence(snap, lens.bar, f.tab_key, &f.tab_label),
                focus: lens.focus.word(),
                focus_words: crate::stats_lens::readout(snap, lens.focus),
                bounds: Some(aterm_a11y::model::Rect {
                    x0: ox as f64 + f64::from(lg.dx),
                    y0: oy as f64 + f64::from(lg.dy),
                    x1: ox as f64 + f64::from(lg.dx) + f64::from(lg.w),
                    y1: oy as f64 + f64::from(lg.dy) + f64::from(lg.h),
                }),
            }
        });
        Some(crate::a11y_tree::GridStats {
            groups: stats_a11y_groups(snap, f.tab_key, &f.tab_label, &self.stats_marks()),
            bounds: Some(bounds),
            lens,
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use aterm_vitals::model::{
        ClusterSpec, GpuSample, MemSample, Pressure, ProcSample, Sample, TabRoot, Ticks,
        VolumeSample,
    };

    /// Gives this test thread's windows the stats bars, as glass gives a
    /// real window ([`stats_drawn_on`]), until the guard drops. For the lens's
    /// behaviour tests, which are unix's.
    #[cfg(unix)]
    #[must_use]
    pub(crate) fn attach_stats() -> Attached {
        TESTS_ATTACHED.with(|c| c.set(true));
        Attached
    }

    /// [`attach_stats`]'s guard: dropping it takes the bars away again.
    #[cfg(unix)]
    pub(crate) struct Attached;

    #[cfg(unix)]
    impl Drop for Attached {
        fn drop(&mut self) {
            TESTS_ATTACHED.with(|c| c.set(false));
        }
    }

    /// Only the Deck takes room: the head the grid fit reads is the same for
    /// the sill and off, so the config apply's re-grid predicate agrees with
    /// the head for every pair of forms, and a sill ↔ off change re-grids
    /// nothing.
    #[test]
    fn only_a_change_into_or_out_of_the_deck_moves_the_head() {
        for a in StatsBars::ALL {
            for b in StatsBars::ALL {
                let moved = chrome_head_px(28.0, 2.0, a) != chrome_head_px(28.0, 2.0, b);
                assert_eq!(a.takes_room() != b.takes_room(), moved, "{a:?} to {b:?}");
            }
        }
        assert_eq!(
            chrome_head_px(28.0, 2.0, StatsBars::Sill),
            chrome_head_px(28.0, 2.0, StatsBars::Off)
        );
    }

    /// A snapshot after three readings: ten cores in two clusters, memory, a
    /// GPU, a volume, aterm with one tab running claude. Shared with the
    /// sill's and the lens's tests.
    pub(crate) fn snapshot() -> Snapshot {
        let mut v = aterm_vitals::Vitals::new();
        let tabs = vec![TabRoot {
            key: 7,
            shell_pid: 200,
            label: "Tab 1".into(),
            program: "claude".into(),
        }];
        let mk = |t: u64, busy: &[u64], cpu: &[u64]| Sample {
            at_ns: t * 1_000_000_000,
            cores: Some(
                busy.iter()
                    .map(|&b| Ticks {
                        busy: b,
                        total: t * 100,
                    })
                    .collect(),
            ),
            clusters: vec![
                ClusterSpec {
                    name: "Super".into(),
                    logical: vec![0, 1, 2, 3],
                },
                ClusterSpec {
                    name: "Efficiency".into(),
                    logical: vec![4, 5, 6, 7, 8, 9],
                },
            ],
            mem: Some(MemSample {
                total: 32 << 30,
                app: 12 << 30,
                wired: 4 << 30,
                compressed: Some(1 << 30),
                cached: 8 << 30,
                free: 7 << 30,
                swap_used: 0,
                swap_total: 0,
                pressure: Pressure::Normal,
                commit: None,
            }),
            gpu: Some(GpuSample {
                device_pct: Some(12),
                cores: Some(10),
                name: None,
            }),
            volume: Some(VolumeSample {
                total: 2_000_000_000_000,
                free: 1_810_000_000_000,
                available: 1_810_000_000_000,
                mount: "/".into(),
            }),
            procs: Some(vec![
                ProcSample {
                    pid: 100,
                    ppid: 1,
                    cpu_ns: Some(cpu[0] * 1_000_000),
                    footprint: Some(300 << 20),
                    name: "aterm".into(),
                },
                ProcSample {
                    pid: 200,
                    ppid: 100,
                    cpu_ns: Some(0),
                    footprint: Some(5 << 20),
                    name: "zsh".into(),
                },
                ProcSample {
                    pid: 201,
                    ppid: 200,
                    cpu_ns: Some(cpu[1] * 1_000_000),
                    footprint: Some(400 << 20),
                    name: "claude".into(),
                },
            ]),
            self_pid: 100,
            tabs: tabs.clone(),
        };
        let _ = v.absorb(&mk(10, &[0; 10], &[0, 0]));
        let _ = v.absorb(&mk(11, &[20, 30, 10, 0, 5, 5, 5, 5, 5, 5], &[100, 400]));
        v.absorb(&mk(
            12,
            &[120, 130, 60, 10, 10, 10, 10, 10, 10, 10],
            &[200, 1200],
        ))
    }

    pub(crate) fn dark_inks() -> VitalsInks {
        aterm_vitals::ink::vitals_inks([0x30, 0x31, 0x35], [0xF8, 0xF8, 0xF2], [0x50, 0xFA, 0x7B])
    }

    pub(crate) fn input(snap: &Snapshot) -> DeckInput<'_> {
        DeckInput {
            snap: Some(snap),
            tab_key: Some(7),
            tab_label: "Tab 1".into(),
            tab_count: 1,
            inks: dark_inks(),
            seam: [0x50, 0x50, 0x55],
            dwc: false,
            marks: Vec::new(),
            plate: None,
        }
    }

    const GEOM: DeckGeom = DeckGeom {
        w: 2238.0,
        h: 124.0,
        pad: 24.0,
        scale: 2.0,
        font_px: 26.0,
    };

    fn texts(prims: &[DrawPrim]) -> Vec<String> {
        prims
            .iter()
            .filter_map(|p| match p {
                DrawPrim::Text { s, .. } => Some(s.clone()),
                _ => None,
            })
            .collect()
    }

    /// Where the Deck sits in the head: under macOS's titlebar at the head's
    /// foot, as it always has; where the strip is the window's band (Linux,
    /// Windows) at the head's top, with the band's lift less the Deck so the
    /// band keeps its whole height under it (the Linux lane's
    /// `proof-20261019-174123` found it over the band's middle). The Linux
    /// arm is held live by the lane's Deck rows (`lab linux deck`).
    #[cfg(unix)]
    #[test]
    fn the_deck_takes_the_heads_foot_under_a_titlebar_and_its_top_over_a_band() {
        let _attached = attach_stats();
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.config
            .sys
            .get_or_insert_with(Default::default)
            .stats_bars = Some("deck".into());
        let deck = deck_px(StatsBars::Deck, app.windows[&wid].scale);
        let head = deck + 14;
        app.windows.get_mut(&wid).expect("window").metrics.head = head;
        app.backend.set_head(head);
        let pad_top = app.win_pad_top(wid);
        let rows = app.deck_rows_px(wid).expect("the head holds the Deck");
        assert_eq!(rows.len(), deck);
        let (want, band_top) = if crate::tab_bar::STRIP_IS_CHROME_BAND {
            (0..deck, pad_top + head - deck)
        } else {
            (head - deck..head, pad_top + head)
        };
        assert_eq!(rows, want);
        assert_eq!(app.strip_band_top_px(wid), band_top);
        let f = app.deck_frame(wid).expect("the Deck draws");
        assert_eq!(
            f.dy as usize, rows.start,
            "the card is drawn where its rows are"
        );
        // A head that cannot hold it, and the Deck off: no rows, and the band
        // lifted by the whole head.
        app.windows.get_mut(&wid).expect("window").metrics.head = deck - 1;
        app.backend.set_head(deck - 1);
        assert_eq!(app.deck_rows_px(wid), None);
        assert_eq!(app.strip_band_top_px(wid), pad_top + deck - 1);
        app.config
            .sys
            .get_or_insert_with(Default::default)
            .stats_bars = Some("off".into());
        app.windows.get_mut(&wid).expect("window").metrics.head = head;
        app.backend.set_head(head);
        assert_eq!(app.deck_rows_px(wid), None);
        assert_eq!(app.strip_band_top_px(wid), pad_top + head);
    }

    /// A point over the Deck is not a strip column, though a point above the
    /// grid clamps onto the strip's row: a press there is the Deck's
    /// (`App::stats_press`), and a pointer resting on it lights no tab and
    /// steps no strip drag. The same point with the Deck off is the strip's,
    /// so the exclusion is what answers `None`.
    #[cfg(unix)]
    #[test]
    fn a_point_over_the_deck_is_no_strip_column() {
        let _attached = attach_stats();
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.tab_strip_rows = 1;
        app.splice_tab_strip(wid);
        app.config
            .sys
            .get_or_insert_with(Default::default)
            .stats_bars = Some("deck".into());
        let deck = deck_px(StatsBars::Deck, app.windows[&wid].scale);
        let head = deck + 14;
        app.windows.get_mut(&wid).expect("window").metrics.head = head;
        app.backend.set_head(head);
        let rows = app.deck_rows_px(wid).expect("the head holds the Deck");
        let (cw, ch) = app.win_cell_size(wid);
        let (fx, fy) = app.frame_origin(wid);
        let col = 3u16;
        let x = fx as f64 + (app.win_pad(wid) + usize::from(col) * cw + cw / 2) as f64;
        let over_deck = fy as f64 + (rows.start + rows.len() / 2) as f64;
        let on_strip = fy as f64 + (app.win_pad_top(wid) + head + ch / 2) as f64;
        assert_eq!(app.strip_col_at(wid, x, over_deck), None, "over the Deck");
        assert_eq!(app.strip_col_at(wid, x, on_strip), Some(col), "the strip");
        app.config
            .sys
            .get_or_insert_with(Default::default)
            .stats_bars = Some("off".into());
        assert_eq!(app.deck_rows_px(wid), None);
        assert_eq!(
            app.strip_col_at(wid, x, over_deck),
            Some(col),
            "control: with the Deck off the same point clamps onto the strip"
        );
    }

    #[test]
    fn the_deck_is_62_points_tall_and_the_sill_and_off_take_no_headroom() {
        assert_eq!(deck_px(StatsBars::Deck, 2.0), 124);
        assert_eq!(deck_px(StatsBars::Deck, 1.0), 62);
        assert_eq!(deck_px(StatsBars::Off, 2.0), 0);
        assert_eq!(deck_px(StatsBars::Sill, 2.0), 0);
        assert_eq!(StatsBars::parse(None), StatsBars::Deck);
        assert_eq!(StatsBars::parse(Some("off")), StatsBars::Off);
        assert_eq!(StatsBars::parse(Some("sill")), StatsBars::Sill);
        assert_eq!(StatsBars::parse(Some(" Sill ")), StatsBars::Sill);
        assert_eq!(StatsBars::parse(Some("nonsense")), StatsBars::Deck);
        for f in StatsBars::ALL {
            assert_eq!(StatsBars::from_word(f.word()), Some(f));
            assert_eq!(StatsBars::parse(Some(f.word())), f);
        }
    }

    #[test]
    fn the_deck_draws_every_core_and_all_three_scopes() {
        let snap = snapshot();
        let prims = deck_prims(&GEOM, &input(&snap));
        let texts = texts(&prims);
        for want in [
            "Global",
            "aterm",
            "Tab 1",
            "S",
            "\u{00D7}4",
            "E",
            "\u{00D7}6",
            "GPU",
            "12%",
        ] {
            assert!(
                texts.iter().any(|t| t == want),
                "{want:?} missing from {texts:?}"
            );
        }
        assert!(texts.iter().any(|t| t.contains("/10")), "{texts:?}");
        assert!(texts.iter().any(|t| t.starts_with("17.0/32G")), "{texts:?}");
        assert!(texts.iter().any(|t| t == "disk 1.81 TB free"), "{texts:?}");
        assert!(texts.iter().any(|t| t == "claude"), "{texts:?}");
        // Everything stays inside the card.
        for p in &prims {
            if let DrawPrim::Panel { x, y, w, h, .. } = p {
                assert!(
                    *x + *w <= GEOM.w + 40.0 && *y + *h <= GEOM.h + 40.0,
                    "{p:?}"
                );
            }
        }
    }

    /// aterm's share of what is busy always carries its noun: a bare
    /// percentage beside a cores figure reads as the machine's load (the
    /// audit's finding 5).
    #[test]
    fn a_share_is_never_a_bare_percentage() {
        let snap = snapshot();
        let texts = texts(&deck_prims(&GEOM, &input(&snap)));
        assert!(texts.iter().any(|t| t.ends_with("% of busy")), "{texts:?}");
        assert!(
            !texts
                .iter()
                .any(|t| t.ends_with('%') && t.contains('c') && t.contains(' ')),
            "{texts:?}"
        );
    }

    /// The GPU's label is set in its own column, like a cluster's, so it can
    /// never overlap the core tiles or the tank (the audit's finding 2).
    #[test]
    fn the_gpu_label_never_overlaps_a_tile_or_the_tank() {
        let snap = snapshot();
        for w in [2238.0, 1698.0, 1248.0, 849.0] {
            for scale in [2.0, 1.0] {
                let g = DeckGeom {
                    w: w * scale / 2.0,
                    h: 62.0 * scale,
                    pad: 12.0 * scale,
                    scale,
                    font_px: 12.0 * scale,
                };
                let (prims, hits) = deck_paint(&g, &input(&snap));
                let gpu = hits
                    .iter()
                    .find(|h| h.instrument == Instrument::Gpu)
                    .expect("a GPU hit");
                let cores_end = hits
                    .iter()
                    .filter(|h| h.instrument == Instrument::Cores && h.bar == Bar::Global)
                    .map(|h| h.rect.0 + h.rect.2)
                    .fold(0.0f32, f32::max)
                    .min(gpu.rect.0 + 1.0);
                let label_x = prims
                    .iter()
                    .find_map(|p| match p {
                        DrawPrim::Text { s, x, .. } if s == "GPU" => Some(*x),
                        _ => None,
                    })
                    .expect("GPU label");
                assert!(
                    label_x >= cores_end - 1.0,
                    "w {w} s {scale}: {label_x} < {cores_end}"
                );
                let pct_w = crate::tray_raster::measure_text(
                    "12%",
                    crate::stats_paint::stats_type(g.font_px, scale).1.get(),
                    crate::widget::TextWeight::Regular,
                );
                // The tank starts after the label column.
                let tank_x = prims
                    .iter()
                    .find_map(|p| match p {
                        DrawPrim::Panel { x, w, h, .. }
                            if *x > label_x && *h > *w * 1.4 && *x <= gpu.rect.0 + gpu.rect.2 =>
                        {
                            Some(*x)
                        }
                        _ => None,
                    })
                    .expect("tank");
                assert!(
                    tank_x >= label_x + pct_w,
                    "w {w} s {scale}: tank {tank_x} label {label_x}+{pct_w}"
                );
            }
        }
    }

    /// Each cluster fills row-major, so a narrow window never shows a ragged
    /// cluster (four cores as 2 / 1 / 1; the audit's finding 6).
    #[test]
    fn a_narrow_window_never_shows_a_ragged_cluster() {
        let snap = snapshot();
        let g = DeckGeom { w: 1248.0, ..GEOM };
        let prims = deck_prims(&g, &input(&snap));
        // Every core tile's top-left; group by row.
        let mut rows: std::collections::BTreeMap<i32, usize> = std::collections::BTreeMap::new();
        let tile = prims
            .iter()
            .filter_map(|p| match p {
                DrawPrim::Stroke { w, h, .. } if (*w - *h).abs() < 0.5 && *w >= 16.0 => Some(*w),
                _ => None,
            })
            .next()
            .expect("a crisp tile rim");
        for p in &prims {
            if let DrawPrim::Stroke { x, y, w, h, .. } = p
                && *w == tile
                && *h == tile
                && *x < g.w / 2.0
            {
                *rows.entry(y.round() as i32).or_default() += 1;
            }
        }
        // Rows in the S cluster have equal counts: no row holds a lone tile
        // while another holds two.
        let counts: Vec<usize> = rows.values().copied().collect();
        assert!(counts.iter().all(|&c| c >= 2), "{rows:?}");
    }

    /// A machine as Linux or Windows reads it: `specs` over `n` cores, two
    /// readings a second apart, with `mem`.
    fn machine(specs: &[ClusterSpec], n: u64, mem: MemSample) -> Snapshot {
        let mut v = aterm_vitals::Vitals::new();
        let mk = |t: u64| Sample {
            at_ns: t * 1_000_000_000,
            cores: Some(
                (0..n)
                    .map(|i| Ticks {
                        busy: t * (i % 50),
                        total: t * 100,
                    })
                    .collect(),
            ),
            clusters: specs.to_vec(),
            mem: Some(mem),
            gpu: None,
            volume: None,
            procs: None,
            self_pid: 1,
            tabs: Vec::new(),
        };
        let _ = v.absorb(&mk(10));
        v.absorb(&mk(11))
    }

    /// Where a text prim's ink ends: its face's own measure.
    fn text_right(p: &DrawPrim) -> f32 {
        let DrawPrim::Text {
            x,
            s,
            px,
            weight,
            face,
            ..
        } = p
        else {
            unreachable!("a text prim");
        };
        x + match face {
            crate::widget::TextFace::Mono => crate::tray_raster::measure_text(s, *px, *weight),
            _ => crate::tray_raster::ui_text_width_for(*face, s, *px),
        }
    }

    /// Each of `labels` is drawn, and ends left of its cluster's first tile
    /// (a square in the track ink: every tile is laid on one).
    fn labels_clear_their_tiles(prims: &[DrawPrim], labels: &[&str]) {
        let track = rgba(dark_inks().track, 0xFF);
        for want in labels {
            let at = prims
                .iter()
                .position(|p| matches!(p, DrawPrim::Text { s, .. } if s == want))
                .unwrap_or_else(|| panic!("{want:?} missing"));
            let right = text_right(&prims[at]);
            let tile_x = prims[at..]
                .iter()
                .find_map(|p| match p {
                    DrawPrim::Panel { x, w, h, fill, .. }
                        if (w - h).abs() < 0.5 && *fill == track =>
                    {
                        Some(*x)
                    }
                    _ => None,
                })
                .expect("a tile after the label");
            assert!(
                right <= tile_x,
                "{want:?} ends at {right}, its tile at {tile_x}"
            );
        }
    }

    /// No two words on one baseline overlap.
    fn words_never_overlap(prims: &[DrawPrim]) {
        let spans: Vec<(f32, f32, f32, &str)> = prims
            .iter()
            .filter_map(|p| match p {
                DrawPrim::Text { x, baseline, s, .. } => {
                    Some((*baseline, *x, text_right(p), s.as_str()))
                }
                _ => None,
            })
            .collect();
        for (i, a) in spans.iter().enumerate() {
            for b in &spans[i + 1..] {
                if (a.0 - b.0).abs() < 0.5 {
                    assert!(a.2 <= b.1 || b.2 <= a.1, "{a:?} overlaps {b:?}");
                }
            }
        }
    }

    fn mem_of(commit: Option<aterm_vitals::CommitSample>) -> MemSample {
        MemSample {
            total: 64 << 30,
            app: 20 << 30,
            wired: 2 << 30,
            cached: 30 << 30,
            free: 12 << 30,
            pressure: Pressure::Normal,
            commit,
            ..MemSample::default()
        }
    }

    /// Linux workstations whose cores are alike: one "CPU" cluster of 32,
    /// and of 128 (a Threadripper's "×128" is wider than a Mac's "E×6"),
    /// each label clear of its tiles. A NEGATIVE CONTROL: Linux's heuristic
    /// commit past its advisory limit is never said.
    #[test]
    fn a_linux_machines_one_cpu_cluster_labels_clear_of_its_tiles() {
        let commit = aterm_vitals::CommitSample {
            charge: 90 << 30,
            limit: 60 << 30,
            exact: false,
        };
        for n in [32u16, 128] {
            let specs = [ClusterSpec {
                name: "CPU".into(),
                logical: (0..n).collect(),
            }];
            let snap = machine(&specs, u64::from(n), mem_of(Some(commit)));
            let prims = deck_prims(&GEOM, &input(&snap));
            labels_clear_their_tiles(&prims, &["CPU", &format!("\u{00D7}{n}")]);
            words_never_overlap(&prims);
            assert!(
                !texts(&prims).iter().any(|s| s.contains("commit")),
                "a heuristic commit is never said"
            );
        }
    }

    /// A Windows laptop split by efficiency class (8 P, 16 E) with its commit
    /// charge at 93 % of the limit: the labels clear their tiles, and the
    /// Global bar says the commit in the warn ink without running into
    /// another word. A NEGATIVE CONTROL: at 89 % it is not said.
    #[test]
    fn a_windows_laptop_labels_its_classes_and_says_its_commit() {
        let specs = [
            ClusterSpec {
                name: "Performance".into(),
                logical: (0..8).collect(),
            },
            ClusterSpec {
                name: "Efficiency".into(),
                logical: (8..24).collect(),
            },
        ];
        let at = |charge| {
            let mem = MemSample {
                pressure: Pressure::Warn,
                swap_used: 3 << 30,
                ..mem_of(Some(aterm_vitals::CommitSample {
                    charge,
                    limit: 100,
                    exact: true,
                }))
            };
            machine(&specs, 24, mem)
        };
        let warn = rgba(dark_inks().warn, 0xFF);
        for g in [GEOM, DeckGeom { w: 1698.0, ..GEOM }] {
            let snap = at(93);
            let prims = deck_prims(&g, &input(&snap));
            labels_clear_their_tiles(&prims, &["P", "\u{00D7}8", "E", "\u{00D7}16"]);
            words_never_overlap(&prims);
            assert!(
                prims.iter().any(|p| matches!(
                    p,
                    DrawPrim::Text { s, color, .. } if s.starts_with("commit 93%") && *color == warn
                )),
                "the commit is said: {:?}",
                texts(&prims)
            );
            let snap = at(89);
            let prims = deck_prims(&g, &input(&snap));
            assert!(
                !texts(&prims).iter().any(|s| s.contains("commit")),
                "under the warn line the commit is not said"
            );
        }
    }

    #[test]
    fn the_same_snapshot_lays_the_same_pixels() {
        let snap = snapshot();
        let a = deck_prims(&GEOM, &input(&snap));
        let b = deck_prims(&GEOM, &input(&snap));
        assert_eq!(prims_fp(&a), prims_fp(&b));
    }

    #[test]
    fn before_the_first_snapshot_the_deck_says_it_is_reading() {
        let empty = Snapshot::default();
        let mut i = input(&empty);
        i.snap = None;
        let (prims, hits) = deck_paint(&GEOM, &i);
        // Words, and no instrument a pointer could rest on yet.
        assert!(prims.iter().any(|p| matches!(p, DrawPrim::Text { .. })));
        assert!(hits.is_empty(), "{hits:?}");
    }

    #[test]
    fn a_110_column_window_keeps_the_disk_the_tab_program_and_the_sparkline() {
        // 110 columns at 2x, as a lab instance and a half-screen window are:
        // the lanes give up width before the words do, and the trace stays.
        let snap = snapshot();
        let g = DeckGeom { w: 1698.0, ..GEOM };
        let (prims, hits) = deck_paint(&g, &input(&snap));
        let texts: Vec<String> = prims
            .iter()
            .filter_map(|p| match p {
                DrawPrim::Text { s, x, .. } => {
                    assert!(*x < g.w, "{s:?} at {x}");
                    Some(s.clone())
                }
                _ => None,
            })
            .collect();
        assert!(texts.iter().any(|t| t.contains("disk")), "{texts:?}");
        assert!(texts.iter().any(|t| t.contains("claude")), "{texts:?}");
        assert!(hits.iter().any(|h| h.instrument == Instrument::Disk));
        assert!(
            hits.iter()
                .filter(|h| h.instrument == Instrument::Memory)
                .count()
                == 3
        );
        // The sparkline: a translucent track per bar beside the CPU figure.
        let track = a(input(&snap).inks.track, 0.55);
        assert_eq!(
            prims
                .iter()
                .filter(|p| matches!(p, DrawPrim::Panel { fill, .. } if *fill == track))
                .count(),
            3,
            "one trace per bar"
        );
    }

    #[test]
    fn a_narrow_window_still_lays_out_inside_the_card_and_never_cuts_a_word() {
        let snap = snapshot();
        for w in [700.0, 900.0, 1248.0] {
            let g = DeckGeom { w, ..GEOM };
            let prims = deck_prims(&g, &input(&snap));
            for p in &prims {
                if let DrawPrim::Text { x, s, .. } = p {
                    assert!(*x < g.w, "{p:?}");
                    assert!(!s.ends_with('\u{2026}'), "a clause folds whole: {s:?}");
                }
            }
        }
    }

    /// Each bar's label carries its key: the Global bar's is the load ramp,
    /// aterm's and the tab's their scope inks, so a newcomer reads which ink
    /// is whose without a legend (the audit's finding 4).
    #[test]
    fn every_bar_label_carries_its_ink_as_a_key() {
        let snap = snapshot();
        let inks = dark_inks();
        let prims = deck_prims(&GEOM, &input(&snap));
        for ink in [inks.aterm, inks.tab, inks.load[4], inks.load[0]] {
            assert!(
                prims
                    .iter()
                    .any(|p| matches!(p, DrawPrim::Panel { fill, w, .. }
                    if fill[..3] == ink[..] && *w <= 6.0)),
                "{ink:?}"
            );
        }
    }

    /// A pinned machine fills the capacity bracket in the ramp's hottest ink.
    #[test]
    fn a_pinned_machine_fills_the_capacity_bracket() {
        let mut snap = snapshot();
        if let Some(cpu) = snap.cpu.as_mut() {
            cpu.busy = TrendView::flat(10.0);
        }
        snap.global.cores = TrendView::flat(10.0);
        let inks = dark_inks();
        let hot = rgba(inks.load[4], 0xFF);
        let count = |snap: &Snapshot| {
            deck_prims(&GEOM, &input(snap))
                .iter()
                .filter(|p| matches!(p, DrawPrim::Panel { fill, w, h, .. } if *fill == hot && *w <= 2.0 && *h > 10.0))
                .count()
        };
        assert_eq!(count(&snap), 1);
        assert_eq!(count(&snapshot()), 0);
    }

    /// A reader meets three named groups of meters, the core row a grid of
    /// per-core meters named stably ("Super core 1"), the values in range.
    #[cfg(a11y_tree)]
    #[test]
    fn the_bars_read_as_groups_of_meters_and_a_grid_of_cores() {
        let snap = snapshot();
        let stuck = StatsMark::new(WellMark::Stuck, 1);
        let groups = stats_a11y_groups(&snap, Some(7), "Tab 1", &[stuck]);
        assert_eq!(groups.len(), 3, "one group a bar");
        // Each group is described by the one words function the lens and
        // the bars share, so a reader hears what a sighted person reads.
        assert_eq!(groups[0].sentence, words::global_sentence(&snap));
        assert_eq!(groups[1].sentence, words::aterm_sentence(&snap));
        assert_eq!(
            groups[2].sentence,
            crate::stats_lens::sentence(&snap, Bar::Tab, Some(7), "Tab 1")
        );
        let global = &groups[0];
        assert_eq!(global.meters.len(), 4, "CPU, GPU, memory and disk");
        assert_eq!(
            global.meters[0].max, 10.0,
            "the CPU meter's range is the cores"
        );
        // One meter a core, each named apart and stably (only values move).
        assert_eq!(global.cores.len(), 10);
        let core_names: std::collections::BTreeSet<&str> =
            global.cores.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(core_names.len(), 10, "every core's name is its own");
        let again = stats_a11y_groups(&snap, Some(7), "Tab 1", &[stuck]);
        assert_eq!(again[0].cores, global.cores);
        for m in global.meters.iter().chain(&global.cores) {
            assert!((m.min..=m.max).contains(&m.value), "{m:?}");
            assert!(!m.words.is_empty(), "{m:?}");
        }
        // The marks ride on the aterm group only, in their own words.
        assert_eq!(groups[1].marks, Some(stuck.words()));
        assert!(groups[0].marks.is_none() && groups[2].marks.is_none());
    }

    #[test]
    fn marks_lead_the_aterm_bar_and_hit_as_jobs() {
        let snap = snapshot();
        let mut i = input(&snap);
        i.marks = vec![
            StatsMark::new(WellMark::Stuck, 1),
            StatsMark::new(WellMark::Quiet, 2),
        ];
        let (prims, hits) = deck_paint(&GEOM, &i);
        assert!(hits.iter().any(|h| h.instrument == Instrument::Jobs));
        assert!(prims.iter().any(|p| matches!(p, DrawPrim::Ring { .. })));
    }
}
