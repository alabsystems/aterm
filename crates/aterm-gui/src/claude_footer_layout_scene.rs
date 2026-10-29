// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit private-dev gallery: synthetic states through the production footer composition.

use super::*;
use crate::autopilot::{HarnessSwitch, SwitchOff};
use aterm_effects::harness_card::CardPalette;
use aterm_phase::Program;

/// Metadata from the measured ALab image; this scene does not create a live upgrade offer.
const PRODUCT: &str = "0.1.0-dev.13764+codex.0.164.0.g4b3a367a0";

#[derive(Default)]
struct Layouts {
    footers: [PaneFooter; 9],
    marks: MarkContents,
    rings: RingContents,
}

thread_local! {
    /// A gallery, like a live pane, retains composed cells and sprite identities across frames.
    /// Boxed lazily so idle threads do not carry 30 KB in the image's TLS template.
    static LAYOUTS: std::cell::RefCell<Option<Box<Layouts>>> = const { std::cell::RefCell::new(None) };
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
    for row in input.grid_top_row..input.grid_bot_row.min(input.cells.len()) {
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
            "Synthetic: Codex product at rest",
            Program::Codex,
            HoverAt::None,
            PRODUCT,
            UpState::None,
        ),
        (
            "Synthetic: Codex product label hover",
            Program::Codex,
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
            Program::Codex,
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
        let title_row = input.grid_top_row + 2 + i * 4;
        let frame_row = title_row + 1;
        if frame_row >= input.grid_bot_row || frame_row >= input.cells.len() {
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
            ch: if program == Program::Codex {
                ' '
            } else {
                '\u{2500}'
            },
            fg: mix(blank.fg, blank.bg, 0.6),
            ..blank
        };
        let key = TopKey {
            frame_row,
            term_row: frame_row - input.grid_top_row,
            line: if program == Program::Codex {
                TopLine::Slab
            } else {
                TopLine::Rule
            },
            run: TopRun {
                col: 2,
                len: input.cols.saturating_sub(4),
                rule,
                voice: None,
                tail: None,
                end: input.cols.saturating_sub(2),
            },
            program,
            build: if program == Program::Codex {
                Build::Alab
            } else {
                Build::Vendor
            },
            switch: Some(switch),
            palette: Some(if harness_off || program == Program::Muse {
                CardPalette::Off
            } else {
                CardPalette::Cruise
            }),
            blank,
            mark_fg: [0xd7, 0x77, 0x57],
            face: KeyFace::default(),
            cell: (cw, ch),
            clip: aterm_core::render::PxRect::EVERYWHERE,
            mark_hover: false,
            title_gen: 0,
            chip: TopChip::NONE,
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
        input
            .fx_layers
            .extend(top.mark.iter().chain(top.ring.iter()).cloned());
    }
    paint_chip_and_effort_samples(layouts, input, blank);
}

/// Bottom-rule chips and mode rows use the same composers as a live Claude pane.
fn paint_chip_and_effort_samples(
    layouts: &mut Layouts,
    input: &mut RenderInput,
    blank: RenderCell,
) {
    let base = input.grid_top_row;
    if base + 35 >= input.grid_bot_row || base + 35 >= input.cells.len() {
        return;
    }
    let title = |input: &mut RenderInput, row: usize, text: &str| {
        for (cell, ch) in input.cells[base + row].iter_mut().zip(text.chars()) {
            cell.ch = ch;
        }
    };
    title(
        input,
        30,
        "Synthetic: worded Autopilot and permission chips, separated by rule",
    );
    let rule = RenderCell {
        ch: '─',
        fg: mix(blank.fg, blank.bg, 0.6),
        ..blank
    };
    let block = crate::claude_lights::Block::layout_scene(blank, [0xd7, 0x77, 0x57]);
    let footer = &mut layouts.footers[7];
    footer.compose(
        input.cols.saturating_sub(4),
        Some(&block),
        rule,
        blank,
        [0xd7, 0x77, 0x57],
        None,
    );
    input.cells[base + 31].fill(rule);
    for (out, cell) in input.cells[base + 31]
        .iter_mut()
        .skip(2)
        .zip(&footer.rule.paint.cells)
    {
        *out = *cell;
    }
    paint_effort_sample(&mut layouts.footers[8], input, blank, rule);
}

/// An explicitly admitted synthetic hint fills open facts through the production card/facts
/// projection and rule composer; the production App's ownership gate is tested separately.
fn paint_effort_sample(
    footer: &mut PaneFooter,
    input: &mut RenderInput,
    blank: RenderCell,
    rule: RenderCell,
) {
    let base = input.grid_top_row;
    for (cell, ch) in input.cells[base + 33]
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
    footer.compose(
        input.cols.saturating_sub(4),
        None,
        rule,
        blank,
        [0xd7, 0x77, 0x57],
        None,
    );
    input.cells[base + 34].fill(rule);
    for (out, cell) in input.cells[base + 34]
        .iter_mut()
        .skip(2)
        .zip(&footer.rule.paint.cells)
    {
        *out = *cell;
    }
    let mode = if footer.shown().effort.is_some() {
        Some(mode)
    } else {
        mode.keeping_effort()
    };
    if let Some(mode) = mode {
        paint_mode_sample(input, blank, &rows, mode, base + 35);
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
mod tests {
    use super::*;

    #[test]
    fn the_private_scene_draws_whole_products_and_actionable_rings_at_actual_cell_size() {
        for (fg, bg) in [("d0d0d0", "111318"), ("202020", "ffffff")] {
            let mut term = aterm_core::terminal::Terminal::new(38, 250);
            term.process(format!("\x1b]10;#{fg}\x07\x1b]11;#{bg}\x07x").as_bytes());
            let mut input = term.cell_frame(38, 250);
            input.grid_bot_row = input.rows;
            let original = input.clone();
            paint(&mut input, (0, 14));
            assert_eq!(input, original, "invalid metrics do not replace the frame");
            paint(&mut input, (7, 14));
            let row = |offset: usize| read_glass(&input.cells[input.grid_top_row + offset]);
            assert!(!row(3).contains(PRODUCT));
            assert!(row(7).contains(PRODUCT));
            assert!(row(11).contains("1 shell"));
            assert!(row(11).contains(UP_ARROW), "the actionable colour arrow");
            for offset in [15, 23] {
                assert!(row(offset).contains('↑'), "a held move's quiet arrow");
                assert!(!row(offset).contains(UP_ARROW), "held is not actionable");
            }
            assert!(row(19).contains(&"7".repeat(CLI_VERSION_MAX - "0.1.0-dev.".len())));
            assert!(
                row(27).contains("updating to") || row(27).contains('→'),
                "asked forms keep the verb or under-way marker"
            );
            assert!(
                row(31).contains("land"),
                "the sample uses a worded Land chip"
            );
            assert!(row(31).contains("plan"), "its permission chip follows it");
            assert!(
                row(31).contains(" ─ "),
                "worded chips are set apart by the rule"
            );
            assert!(
                row(34).contains("Opus 5.5") && row(34).contains("medium"),
                "the parsed hint fills the card's open effort on the composed rule: {:?}",
                row(34)
            );
            assert_eq!(
                row(35).trim_end(),
                "1 shell",
                "effort filled from the admitted hint leaves only jobs"
            );
            let ring = input
                .fx_layers
                .iter()
                .find(|l| l.id == top_ring_id(3))
                .expect("the actionable ready arrow is ringed");
            assert_eq!(ring.offset.1, (input.grid_top_row + 11) as i32 * 14);
            for i in [4, 6] {
                assert!(
                    !input.fx_layers.iter().any(|l| l.id == top_ring_id(i)),
                    "a held arrow has no hover ring"
                );
            }
            assert!(
                !input.fx_layers.iter().any(|l| l.id == top_ring_id(7)),
                "the asked move's one-cell marker has no ring"
            );
            let painted = input.clone();
            paint(&mut input, (7, 14));
            assert_eq!(input, painted, "a second frame does not duplicate sprites");
        }
    }
}
