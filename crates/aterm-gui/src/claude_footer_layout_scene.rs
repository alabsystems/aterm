// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit private-dev gallery: synthetic states through the production footer composition.

use super::*;
use crate::autopilot::{AutopilotView, HarnessSwitch, SwitchOff};
use aterm_agent::harness::quota_scenes;
use aterm_agent::harness::quota_view::QuotaView;
use aterm_effects::harness_card::CardPalette;
use aterm_phase::Program;

/// A sample Claude Code version; this scene does not create a live upgrade offer.
const PRODUCT: &str = "2.1.305";

/// The rows between one top-rule sample's title and the next's.
const TOP_PITCH: usize = 3;

/// One of Claude Code's notices on the tap's row: its words, and the mode row's own words under
/// it, which with the tap's words decide whether the notice keeps that row ([`tap_row`]).
#[derive(Clone, Copy)]
struct Notice<'a> {
    words: &'a str,
    /// The mode row from column 0, Claude's indent included, without any notice of its own.
    mode: &'a str,
}

/// Claude Code's usage-credits notice over the auto mode row, as aterm-phase's
/// `claude-2.1.318-tap-row-week-credits` and `-credits-70col` captures show it.
const CREDITS: Notice<'static> = Notice {
    words: "Now using usage credits",
    mode: "  \u{23f5}\u{23f5} auto mode on (shift+tab to cycle)",
};

/// Claude Code's launch-time effort hint over the launch's mode row, which says `· ← for agents`
/// for the same ten seconds, as aterm-phase's `claude-2.1.318-tap-row-week-launch` capture shows
/// it.
const EFFORT_HINT: Notice<'static> = Notice {
    words: "\u{25d0} medium \u{b7} /effort",
    mode: "  \u{23f5}\u{23f5} auto mode on (shift+tab to cycle) \u{b7} \u{2190} for agents",
};

/// One of the glass design's quota samples (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md`
/// §1.2, §1.3): a title over line D and, where line E is claimed, the quota row under it.
struct QuotaSample {
    /// Its case ([`quota_scenes::cases`]).
    case: &'static str,
    /// Its title's row: line D lies under it, and line E under line D.
    row: usize,
    title: &'static str,
    /// Whether the row under the bottom rule holds the tap's words for the case
    /// ([`quota_scenes::tap_words`]), which the glass claims (`false`: line E is not claimed, so
    /// line D carries the account unit).
    claimed: bool,
    /// Claude's own notice on that row, set where Claude sets it ([`tap_row`]); the glass's
    /// span ends two cells before it (§5.2). A frame too narrow for it on the row lays no sample.
    notice: Option<Notice<'static>>,
}

/// The quota samples, widest first, then the samples whose line E shares its row with one of
/// Claude's notices (§7: "the row ends two cells before a notice").
const QUOTA_SAMPLES: [QuotaSample; 5] = [
    QuotaSample {
        case: "b",
        row: 29,
        title: "Synthetic: case (b), line E claimed: line D without the account, line E the quota row",
        claimed: true,
        notice: None,
    },
    QuotaSample {
        case: "c",
        row: 33,
        title: "Synthetic: case (c), three named accounts: line D without the account, line E the quota row",
        claimed: true,
        notice: None,
    },
    QuotaSample {
        case: "b",
        row: 37,
        title: "Synthetic: case (b), line E not claimed: line D carries the words-only account unit",
        claimed: false,
        notice: None,
    },
    QuotaSample {
        case: "b",
        row: 41,
        title: "Synthetic: case (b), Claude's credits notice on line E's row",
        claimed: true,
        notice: Some(CREDITS),
    },
    QuotaSample {
        case: "b",
        row: 45,
        title: "Synthetic: case (b), Claude's launch effort hint on line E's row",
        claimed: true,
        notice: Some(EFFORT_HINT),
    },
];

/// The row under the bottom rule as Claude Code sets it in a pane `cols` wide: the tap's `words`
/// from column 2 and the notice right-aligned, its last cell at `cols − 3`, where it keeps a blank
/// after the wider of the tap's words and its mode row (§5.2), as every capture of a notice on
/// the tap's row shows it (aterm-phase's `claude-2.1.300-tap-row-80-credits`,
/// `claude-2.1.318-tap-row-week-credits`, `-credits-70col`, `-credits-manual-60col` and
/// `-launch`, and `claude-2.1.319-tap-row-launch-74col`). `None` where it would not: Claude then
/// gives the notice a row of its own (`-credits-50col`; at 59 columns under manual mode; the
/// launch hint at 73 columns and below, beside the launch's 53-cell mode row, as
/// `claude-2.1.319-tap-row-launch-73col` shows it under the mode row).
fn tap_row(words: &str, notice: Option<Notice<'_>>, cols: usize) -> Option<String> {
    let words = format!("  {words}");
    let Some(notice) = notice else {
        return Some(words);
    };
    let at = cols.checked_sub(2 + notice.words.chars().count())?;
    let wider = words.chars().count().max(notice.mode.chars().count());
    (at > wider).then(|| format!("{words:<at$}{}", notice.words))
}

/// The first footer of the quota samples (the top rules', the chips' and the effort's before).
const QUOTA_FOOTER: usize = 9;

#[derive(Default)]
struct Layouts {
    footers: [PaneFooter; QUOTA_FOOTER + QUOTA_SAMPLES.len()],
    marks: MarkContents,
    rings: RingContents,
    /// The design's cases (`quota_scenes::cases`), replayed once for every frame after.
    cases: Option<Vec<(&'static str, QuotaView)>>,
}

thread_local! {
    /// A gallery, like a live pane, retains composed cells and sprite identities across frames.
    /// Boxed lazily so idle threads do not carry 30 KB in the image's TLS template.
    static LAYOUTS: std::cell::RefCell<Option<Box<Layouts>>> = const { std::cell::RefCell::new(None) };
}

/// The rows the scene lays its samples in: the frame's terminal band where it declares one (a
/// windowed frame's, below its chrome strip), else every row. A windowless capture lays a frame
/// that declares no band (`grid_bot_row` 0, as a split window's), and the scene still fills it.
fn scene_rows(input: &RenderInput) -> std::ops::Range<usize> {
    let rows = input.cells.len();
    if input.grid_bot_row > input.grid_top_row {
        input.grid_top_row.min(rows)..input.grid_bot_row.min(rows)
    } else {
        0..rows
    }
}

/// Replace the private instance's grid with samples, retaining its actual theme and cell metrics.
pub(crate) fn paint(input: &mut RenderInput, cell: (usize, usize)) {
    LAYOUTS.with_borrow_mut(|slot| {
        let layouts = slot.get_or_insert_with(|| Box::new(Layouts::default()));
        paint_with(layouts, input, cell);
    });
}

#[allow(clippy::too_many_lines)]
fn paint_with(layouts: &mut Layouts, input: &mut RenderInput, cell: (usize, usize)) {
    let (Ok(cw), Ok(ch)) = (u16::try_from(cell.0), u16::try_from(cell.1)) else {
        return;
    };
    if cw == 0 || ch == 0 || input.cols < 4 {
        return;
    }
    let blank = RenderCell {
        ch: ' ',
        ..input.implicit_blank
    };
    let band = scene_rows(input);
    for row in band.clone() {
        input.cells[row] = vec![blank; input.cols];
        if let Some(entries) = input.clusters.get_mut(row) {
            entries.clear();
        }
        if let Some(entries) = input.combining.get_mut(row) {
            entries.clear();
        }
        if let Some(entries) = input.images.get_mut(row) {
            entries.clear();
        }
        if let Some(entries) = input.default_bg_spans.get_mut(row) {
            entries.clear();
        }
    }
    input.chrome_rasters.clear();
    input.fx_layers.clear();
    input.cursor_visible = false;
    let mut long = String::from("0.1.0-dev.");
    long.extend(std::iter::repeat_n('7', CLI_VERSION_MAX - long.len()));
    let samples = [
        (
            "Synthetic: Claude Code at rest",
            Program::Claude,
            HoverAt::None,
            PRODUCT,
            UpState::None,
        ),
        (
            "Synthetic: Claude Code label hover",
            Program::Claude,
            HoverAt::Label,
            PRODUCT,
            UpState::None,
        ),
        (
            "Synthetic: Ready arrow, one shell running",
            Program::Claude,
            HoverAt::Arrow,
            "2.1.305",
            UpState::Ready,
        ),
        (
            "Synthetic: Held arrow, harness off",
            Program::Claude,
            HoverAt::Arrow,
            "2.1.305",
            UpState::Held,
        ),
        (
            "Synthetic: 128-byte version label hover",
            Program::Claude,
            HoverAt::Label,
            long.as_str(),
            UpState::None,
        ),
        (
            "Synthetic: Held Muse revision, Autopilot off",
            Program::Muse,
            HoverAt::Arrow,
            "1.4.13-R6427.1",
            UpState::Held,
        ),
        (
            "Synthetic: Asked arrow, restart under way",
            Program::Claude,
            HoverAt::Arrow,
            "2.1.305",
            UpState::Asked,
        ),
    ];
    for (i, (title, program, hover, version, state)) in samples.into_iter().enumerate() {
        let title_row = band.start + 1 + i * TOP_PITCH;
        let frame_row = title_row + 1;
        if frame_row >= band.end {
            break;
        }
        for (cell, ch) in input.cells[title_row].iter_mut().zip(title.chars()) {
            cell.ch = ch;
        }
        let harness_off = state == UpState::Held && program == Program::Claude;
        let face = if harness_off {
            HarnessSwitch::Off(SwitchOff::Agent(program))
        } else {
            HarnessSwitch::On
        };
        let (wait, why) = if harness_off {
            HARNESS_OFF_WORDS
        } else if state == UpState::Held {
            held_words(aterm_agent::harness::upgrade_drive::AUTOPILOT_OFF).unwrap_or_default()
        } else {
            (up_wait("background"), "")
        };
        let agent = aterm_agent::harness::upgrade::Agent::of(program).unwrap_or_default();
        let up = TopUp {
            state,
            agent,
            // As `TopUp::of` reads a row whose lane waited on nothing a press waits for.
            press: agent.presses().then_some(""),
            to: Inline::new(if program == Program::Muse {
                "1.4.13-R6500.1"
            } else {
                "2.1.310"
            }),
            revision: program == Program::Muse,
            wait,
            why,
        };
        let mut switch = SwitchPaint::of(
            face,
            hover,
            Some(program),
            TopCli {
                ver: Inline::new(version),
                up,
                ..TopCli::default()
            },
        );
        if state == UpState::Ready {
            switch = switch.with_jobs("1 shell");
        }
        let rule = RenderCell {
            ch: '\u{2500}',
            fg: mix(blank.fg, blank.bg, 0.6),
            ..blank
        };
        let key = TopKey {
            frame_row,
            term_row: frame_row - band.start,
            run: TopRun {
                col: 2,
                len: input.cols.saturating_sub(4),
                rule,
                voice: None,
                tail: None,
                end: input.cols.saturating_sub(2),
            },
            program,
            build: Build::Vendor,
            switch: Some(switch),
            palette: Some(if harness_off || program == Program::Muse {
                CardPalette::Off
            } else {
                CardPalette::Cruise
            }),
            blank,
            field: PaneField {
                col_off: 0,
                cols: input.cols,
            },
            face: KeyFace::default(),
            cell: (cw, ch),
            clip: aterm_core::render::PxRect::EVERYWHERE,
        };
        layouts.footers[i].session = i as u64 + 1;
        let top = layouts.footers[i].compose_top(key, &[], &mut layouts.marks, &mut layouts.rings);
        for (cell, composed) in input.cells[frame_row]
            .iter_mut()
            .skip(top.col)
            .zip(&top.cells)
        {
            *cell = *composed;
        }
        paint_marks(input, frame_row);
        input
            .fx_layers
            .extend(top.mark.iter().chain(top.ring.iter()).cloned());
    }
    paint_chip_and_effort_samples(layouts, input, blank, &band);
    paint_quota_samples(layouts, input, blank, (cw, ch), &band);
}

/// The glass a sample rule wears under `palette`, laid from frame column 2 to two short of a row
/// `cols` wide, as a pane's composer rule is.
fn scene_glass(
    palette: CardPalette,
    cols: usize,
    rule: RenderCell,
    blank: RenderCell,
) -> (GlassInks, RuleAt) {
    (
        GlassInks::of(
            Some(palette),
            pack_rgb(blank.bg),
            pack_rgb(blank.fg),
            pack_rgb(rule.fg),
        ),
        RuleAt {
            end: cols.saturating_sub(2),
            cols,
        },
    )
}

/// Bottom-rule chips and mode rows use the same composers as a live Claude pane.
fn paint_chip_and_effort_samples(
    layouts: &mut Layouts,
    input: &mut RenderInput,
    blank: RenderCell,
    band: &std::ops::Range<usize>,
) {
    let base = band.start;
    if base + 27 >= band.end {
        return;
    }
    let title = |input: &mut RenderInput, row: usize, text: &str| {
        for (cell, ch) in input.cells[base + row].iter_mut().zip(text.chars()) {
            cell.ch = ch;
        }
    };
    title(
        input,
        22,
        "Synthetic: worded Autopilot and permission chips, separated by rule",
    );
    let rule = RenderCell {
        ch: '─',
        fg: mix(blank.fg, blank.bg, 0.6),
        ..blank
    };
    // Land's chip, worded as a click that just reached it shows it, under Land's sunset.
    let (glass, at) = scene_glass(CardPalette::Land, input.cols, rule, blank);
    let block = crate::claude_lights::Block::layout_scene(
        blank,
        crate::claude_lights::ChipAt {
            glass,
            col: 2 + footer::RULE_LEAD + 1,
            cols: input.cols,
        },
        (
            AutopilotView::Land,
            "  \u{23f8} plan mode on (shift+tab to cycle)",
            true,
        ),
    );
    let footer = &mut layouts.footers[7];
    footer.compose(
        footer::rule_room(&(2..at.end)),
        Some(&block),
        rule,
        blank,
        (glass, at),
        None,
    );
    input.cells[base + 23].fill(rule);
    for (out, cell) in input.cells[base + 23]
        .iter_mut()
        .skip(2)
        .zip(&footer.rule.paint.cells)
    {
        *out = *cell;
    }
    paint_marks(input, base + 23);
    paint_effort_sample(&mut layouts.footers[8], input, (blank, rule), base);
}

/// An explicitly admitted synthetic hint fills open facts through the production card/facts
/// projection and rule composer; the production App's ownership gate is tested separately.
fn paint_effort_sample(
    footer: &mut PaneFooter,
    input: &mut RenderInput,
    (blank, rule): (RenderCell, RenderCell),
    base: usize,
) {
    for (cell, ch) in input.cells[base + 25]
        .iter_mut()
        .zip("Synthetic: admitted hint fills open effort; the mode row keeps jobs".chars())
    {
        cell.ch = ch;
    }
    let rows = vec![
        "─".repeat(input.cols),
        "  ⏵⏵ bypass permissions on · 1 shell  ◐ medium · /effort".to_owned(),
    ];
    let Some(mode) = footer::mode_row_rewrite_under(&rows, 0) else {
        return;
    };
    let Some((_, level)) = mode.effort_said() else {
        return;
    };
    let facts = FooterFacts {
        model: Some("Opus 5.5".to_owned()),
        version: Some("2.1.283".to_owned()),
        effort_for: Some("Opus 5.5".to_owned()),
        effort_open: true,
        ..FooterFacts::default()
    };
    let card = aterm_phase::LaunchCard {
        model: "Opus 5.5".to_owned(),
        version: "2.1.283".to_owned(),
        effort: Some(level.to_owned()),
    };
    footer.note_facts(&facts, Some(&card), true, FactMask::default());
    let (glass, at) = scene_glass(CardPalette::Cruise, input.cols, rule, blank);
    footer.compose(
        footer::rule_room(&(2..at.end)),
        None,
        rule,
        blank,
        (glass, at),
        None,
    );
    input.cells[base + 26].fill(rule);
    for (out, cell) in input.cells[base + 26]
        .iter_mut()
        .skip(2)
        .zip(&footer.rule.paint.cells)
    {
        *out = *cell;
    }
    paint_marks(input, base + 26);
    let mode = if footer.shown().effort.is_some() {
        Some(mode)
    } else {
        mode.keeping_effort()
    };
    if let Some(mode) = mode {
        paint_mode_sample(input, blank, &rows, mode, base + 27);
    }
}

/// The glass design's line D and line E (§1.2, §1.3) for [`QUOTA_SAMPLES`], through the production
/// paths a live Claude pane takes: the facts a pane publishes with its case's quota view, the
/// claim of the row under the bottom rule ([`PaneFooter::claim_quota_row`] over a scratch screen
/// of the rule and that row as Claude sets it, [`tap_row`]: the tap's words and, in the notice
/// samples, Claude's notice), line D's rule composed with line E claimed or not
/// ([`PaneFooter::note_line_e`]), and the quota row and its midnight ticks
/// ([`PaneFooter::compose_quota_row`]) over Claude's cells on that row. The cases' words are said
/// at offset 0, UTC, as [`quota_scenes`] builds them.
fn paint_quota_samples(
    layouts: &mut Layouts,
    input: &mut RenderInput,
    blank: RenderCell,
    cell: (u16, u16),
    band: &std::ops::Range<usize>,
) {
    let base = band.start;
    let Ok(cols16) = u16::try_from(input.cols) else {
        return;
    };
    let rule = RenderCell {
        ch: '\u{2500}',
        fg: mix(blank.fg, blank.bg, 0.6),
        ..blank
    };
    // The design's own samples (§1.2): Cruise's chip, `∞ yolo`, under Cruise's rainbow.
    let (glass, at) = scene_glass(CardPalette::Cruise, input.cols, rule, blank);
    let block = crate::claude_lights::Block::layout_scene(
        blank,
        crate::claude_lights::ChipAt {
            glass,
            col: 2 + footer::RULE_LEAD + 1,
            cols: input.cols,
        },
        (
            AutopilotView::Cruise,
            "  \u{23f5}\u{23f5} bypass permissions on (shift+tab to cycle)",
            false,
        ),
    );
    let cases = layouts.cases.get_or_insert_with(quota_scenes::cases);
    for (i, sample) in QUOTA_SAMPLES.iter().enumerate() {
        let &QuotaSample {
            case,
            row,
            title,
            claimed,
            notice,
        } = sample;
        // Its title, line D and, claimed, line E: a grid too short for them ends the samples.
        let last = base + row + 1 + usize::from(claimed);
        if last >= band.end {
            break;
        }
        let Some(view) = cases.iter().find(|(name, _)| *name == case).map(|(_, v)| v) else {
            continue;
        };
        // The row under the bottom rule, claimed: a notice Claude would not set on it here
        // leaves no sample.
        let tap = match claimed.then(|| tap_row(&quota_scenes::tap_words(view), notice, input.cols))
        {
            Some(None) => continue,
            Some(Some(tap)) => Some(tap),
            None => None,
        };
        for (out, ch) in input.cells[base + row].iter_mut().zip(title.chars()) {
            out.ch = ch;
        }
        let footer = &mut layouts.footers[QUOTA_FOOTER + i];
        footer.session = (QUOTA_FOOTER + i) as u64 + 1;
        let facts = FooterFacts {
            model: Some("Opus 5.5".to_owned()),
            effort: Some("xhigh".to_owned()),
            path: Some("~/aterm".to_owned()),
            branch: Some("main".to_owned()),
            account: Some("main".to_owned()),
            subscription: Some(SubscriptionDisplay {
                tap_own: claimed,
                ..SubscriptionDisplay::default()
            }),
            quota: Some(view.clone()),
            ..FooterFacts::default()
        };
        footer.note_facts(&facts, None, true, FactMask::default());
        let d = base + row + 1;
        // The claim reads the row under the bottom rule as a live pane's splice does, and the row
        // shows Claude's cells, the tap's words and any notice in the pane's ink, under the glass.
        let claim = tap.map_or(quota::Claim::Not, |tap| {
            let rows = ["\u{2500}".repeat(input.cols), tap];
            let mut term = aterm_core::terminal::Terminal::new(3, cols16);
            term.process(format!("{}\r\n{}", rows[0], rows[1]).as_bytes());
            let mut scratch = term.cell_frame(3, input.cols);
            scratch.cursor_visible = false;
            let claudes = &scratch.cells[scratch.grid_top_row + 1];
            for (out, cell) in input.cells[d + 1].iter_mut().zip(claudes) {
                *out = RenderCell {
                    ch: cell.ch,
                    wide: cell.wide,
                    ..blank
                };
            }
            footer.claim_quota_row(&quota::RowAt {
                scratch: &scratch,
                rows: &rows,
                region: PaneTextRegion {
                    strip: scratch.grid_top_row,
                    row_off: 0,
                    col_off: 0,
                    rows: 3,
                    cols: input.cols,
                },
                rule: 0,
                chrome_owned: false,
            })
        });
        footer.note_line_e(matches!(claim, quota::Claim::Laid(_)));
        footer.compose(
            footer::rule_room(&(2..at.end)),
            Some(&block),
            rule,
            blank,
            (glass, at),
            None,
        );
        input.cells[d].fill(rule);
        for (out, composed) in input.cells[d]
            .iter_mut()
            .skip(2)
            .zip(&footer.rule.paint.cells)
        {
            *out = *composed;
        }
        paint_marks(input, d);
        let quota::Claim::Laid(span) = claim else {
            continue;
        };
        let region = PaneTextRegion {
            strip: base,
            row_off: 0,
            col_off: 0,
            rows: band.end - base,
            cols: input.cols,
        };
        let Some(write) = footer.compose_quota_row(
            footer.session,
            0,
            span,
            (region, d + 1),
            (glass, blank, false),
            cell,
            aterm_core::render::PxRect::EVERYWHERE,
        ) else {
            continue;
        };
        for (out, composed) in input.cells[d + 1]
            .iter_mut()
            .skip(write.col)
            .zip(footer.quota.cells())
        {
            *out = *composed;
        }
        paint_marks(input, d + 1);
        input.fx_layers.extend(write.ticks);
    }
}

/// Draws the glass marks among frame row `row`'s cells as the live footer's write does
/// ([`crate::claude_card::WindowCards::paint_marks`], which takes every row the footer writes but
/// a vendor's mode row): each `●`, `◆`, `⌂`, `⎇`, `⎆` of a composed row is its chrome icon, whole
/// strokes in its own cell in the cell's own ink ([`GlassInks::mark`] on line D), and not its
/// font's glyph, which a fallback face draws as a thin outline that reads pale at 1x.
fn paint_marks(input: &mut RenderInput, row: usize) {
    let (Some(cells), Ok(at)) = (input.cells.get(row), u16::try_from(row)) else {
        return;
    };
    let icons: Vec<aterm_core::render::ChromeIcon> =
        crate::claude_card::glass_marks(0, cells).collect();
    if icons.is_empty() {
        return;
    }
    match input
        .chrome_rasters
        .iter_mut()
        .find(|raster| raster.row == at)
    {
        Some(raster) => raster.icons.extend(icons),
        None => input.chrome_rasters.push(aterm_core::render::ChromeRaster {
            row: at,
            ground: Arc::from(Vec::new()),
            rail: Arc::from(Vec::new()),
            rail_h: 0,
            clear_rail: false,
            own: Vec::new(),
            split: None,
            rings: Vec::new(),
            icons,
        }),
    }
}

/// Copy a production mode-row rewrite into one of the synthetic gallery rows.
fn paint_mode_sample(
    input: &mut RenderInput,
    blank: RenderCell,
    rows: &[String],
    mode: footer::ModeRow,
    destination: usize,
) {
    let Ok(cols) = u16::try_from(input.cols) else {
        return;
    };
    let mut term = aterm_core::terminal::Terminal::new(3, cols);
    term.process(format!("{}\r\n{}", rows[0], rows[1]).as_bytes());
    let mut scratch = term.cell_frame(3, input.cols);
    scratch.cursor_visible = false;
    for cell in scratch.cells.iter_mut().flatten() {
        cell.fg = blank.fg;
        cell.bg = blank.bg;
    }
    let region = PaneTextRegion {
        strip: scratch.grid_top_row,
        row_off: 0,
        col_off: 0,
        rows: 3,
        cols: input.cols,
    };
    let (mut writes, mut cells) = (Vec::new(), Vec::new());
    if collect_mode_row(
        &scratch,
        rows,
        region,
        mode,
        |_| false,
        blank,
        &mut writes,
        &mut cells,
    ) && let Some(write) = writes.first()
    {
        for (out, cell) in input.cells[destination]
            .iter_mut()
            .zip(&cells[write.cells.clone()])
        {
            *out = *cell;
        }
    }
}

#[cfg(test)]
#[path = "claude_footer_layout_scene_tests.rs"]
mod tests;
