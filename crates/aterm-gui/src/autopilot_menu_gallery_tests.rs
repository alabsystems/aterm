// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The footer cards' judging gallery (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §1.4,
//! §1.5): every card kind in the one look, laid as the window lays it
//! ([`card_layers`]) and drawn by the CPU renderer, a frame per kind, theme, palette and state,
//! with a manifest, so a set from this build and a set from another can be compared.

use aterm_agent::harness::lights::Mode;
use aterm_agent::supervise::policy::claude_settings::{PermissionDefault, Resolved};
use aterm_effects::harness_card::CardPalette;

use super::*;
use crate::fact_menu::FactMenuFacts;
use crate::footer_card_plate::tests::bundled_cpu_in;
use crate::footer_card_plate::{CardPlate, CardWash};
use crate::permission_box::Told;

const SETTINGS: &str = "~/.claude/settings.json";

fn facts() -> FactMenuFacts {
    FactMenuFacts {
        title: "tab 1".into(),
        room: MAX_CONTENT,
        model: Some("Opus 5.5".into()),
        effort: Some("high".into()),
        path: Some("~/aterm".into()),
        branch: Some("main".into()),
        context: Some("ctx 84% left".into()),
        cwd: Some("/Users//person/aterm".into()),
        ..Default::default()
    }
}

fn pick(
    family: crate::claude_facts::ModelFamily,
    label: &str,
    arg: Option<&str>,
    current: bool,
    note: Option<&str>,
) -> crate::claude_facts::ModelPick {
    crate::claude_facts::ModelPick {
        family,
        label: label.into(),
        arg: arg.map(|arg| crate::claude_slash::ModelArg::new(arg).expect("a `/model` argument")),
        current,
        note: note.map(str::to_owned),
    }
}

/// A card of every kind of row the account's card lays, which its plain state does not hold
/// at once: group headings, named facts in each tone, a choice with trailing words, a checked
/// and a disabled one, a drawn row with a warn and a busy cell, a warn note and a rule.
fn grouped() -> Vec<MenuRow> {
    let fact = |label: &str, value: &str, tone: NoteTone| MenuRow::Fact {
        label: label.into(),
        value: value.into(),
        spoken: format!("{label}: {value}"),
        tone,
    };
    let item = |label: &str, trail: &str, enabled: bool, checked: bool| MenuRow::Item {
        glyph: "\u{2386}",
        label: label.into(),
        spoken: label.into(),
        act: MenuAct::Fact(Fact::Account, FactAct::CopyPath),
        enabled,
        checked,
        trail: trail.into(),
    };
    let plot = |ch: char, ink: PlotInk, busy: bool| PlotCell { ch, ink, busy };
    vec![
        MenuRow::Header("Account".into()),
        fact("Token", "in the vault", NoteTone::Value),
        fact("Budget", "62% of the week left", NoteTone::Label),
        fact("Machine", "out ~Wed 22:00", NoteTone::Warn),
        MenuRow::Section("Budget".into()),
        MenuRow::plot(
            vec![
                plot('\u{2583}', PlotInk::Read, false),
                plot('\u{2585}', PlotInk::Read, false),
                plot('\u{2587}', PlotInk::Read, true),
                plot('\u{2595}', PlotInk::Axis, false),
                plot('\u{2192}', PlotInk::Mark, false),
                plot('\u{2504}', PlotInk::Track, false),
                plot('\u{2504}', PlotInk::Track, false),
                plot('\u{2573}', PlotInk::Warn, false),
                plot('\u{21ba}', PlotInk::Mark, false),
            ],
            "the week as read and as projected",
        ),
        MenuRow::Note {
            text: "\u{2573} out ~Wed 22:00\u{2013}Thu 09:00".into(),
            spoken: "Out from about Wednesday 22:00 until Thursday 09:00.".into(),
            tone: NoteTone::Warn,
        },
        MenuRow::Section("Switch this Mac to".into()),
        item("sol@example.test", "\u{2264}88% left", true, false),
        item("ada@example.test", "\u{2264}41% left", true, true),
        item("eve@example.test", "out until Mon", false, false),
        MenuRow::Rule,
        item("Account settings\u{2026}", "", true, false),
    ]
}

/// Every card kind the footer opens, by name: the model's choices (and one the build does not
/// offer, a choice that cannot be made), the effort's slider, the folder's, the branch's and
/// the context's right-click cards, the account's card as it opens and with every kind of row
/// it lays ([`grouped`]), and the permission box under each thing it tells ([`Told`]).
fn cards() -> Vec<(String, Vec<MenuRow>)> {
    use crate::claude_facts::ModelFamily;
    let of = |fact: Fact, facts: &FactMenuFacts| crate::fact_menu::rows(fact, facts);
    let model = FactMenuFacts {
        picks: Some(vec![
            pick(
                ModelFamily::Fable,
                "Fable 5.1",
                Some("claude-fable-5-1"),
                false,
                Some("billed to usage credits on this plan"),
            ),
            pick(
                ModelFamily::Opus,
                "Opus 5.5",
                Some("claude-opus-5-5"),
                true,
                None,
            ),
        ]),
        ..facts()
    };
    let unoffered = FactMenuFacts {
        picks: Some(vec![
            pick(
                ModelFamily::Fable,
                "Fable",
                None,
                false,
                Some("not offered by this Claude Code build"),
            ),
            pick(
                ModelFamily::Opus,
                "Opus 5.5",
                Some("claude-opus-5-5"),
                true,
                None,
            ),
        ]),
        ..facts()
    };
    let effort = FactMenuFacts {
        stops: Some((vec!["medium", "high", "xhigh"], Some(1))),
        ..facts()
    };
    let account = FactMenuFacts {
        email: Some("person@example.com".into()),
        ..facts()
    };
    let mut out = vec![
        ("model".to_owned(), of(Fact::Model, &model)),
        ("model-unoffered".to_owned(), of(Fact::Model, &unoffered)),
        ("effort".to_owned(), of(Fact::Effort, &effort)),
        ("folder".to_owned(), of(Fact::Path, &facts())),
        ("branch".to_owned(), of(Fact::Branch, &facts())),
        ("context".to_owned(), of(Fact::Context, &facts())),
        ("account".to_owned(), of(Fact::Account, &account)),
        ("account-grouped".to_owned(), grouped()),
    ];
    let unset = PermissionDefault {
        mode: Resolved::Unset,
        bypass_disabled: None,
    };
    let named = Resolved::Mode {
        mode: Mode::Auto,
        source: SETTINGS.to_owned(),
    };
    for (name, told) in [
        ("turning-on", Told::TurningOn),
        ("leaving-unset", Told::Leaving(Resolved::Unset)),
        ("leaving-named", Told::Leaving(named)),
        ("default-is-yolo", Told::DefaultIsYolo(SETTINGS.to_owned())),
        ("bypass-off", Told::BypassOff(SETTINGS.to_owned())),
        (
            "shown-yolo",
            Told::Shown(
                Mode::Bypass,
                Some(crate::permission_box::would(Mode::Bypass, unset.clone())),
            ),
        ),
        (
            "shown-auto",
            Told::Shown(
                Mode::Auto,
                Some(crate::permission_box::would(Mode::Auto, unset.clone())),
            ),
        ),
        ("shown-plan-alone", Told::Shown(Mode::Plan, None)),
    ] {
        let (rows, _) = crate::permission_box::rows(&told, MAX_CONTENT);
        out.push((format!("permission-{name}"), rows));
    }
    out
}

/// The row the pointer or the keys rest on in a lit frame: the card's first choice, on a
/// slider its first stop that can be picked and is not the thumb's.
fn lit(rows: &mut [MenuRow]) -> Option<Washed> {
    let i = rows.iter().position(MenuRow::selectable)?;
    if let MenuRow::Stops {
        stops, at, focus, ..
    } = &mut rows[i]
    {
        *focus = (0..stops.len()).find(|j| stops[*j].enabled && Some(*j) != *at);
        return Some((i, *focus));
    }
    Some((i, None))
}

/// One frame of the gallery.
struct Shot {
    png: Vec<u8>,
    /// The frame's pixels and width.
    pixels: Vec<u32>,
    width: usize,
    /// The card's occluder, in window pixels.
    card: aterm_core::render::PxRect,
    /// The frame's cells, columns by rows, and the card's.
    grid: (usize, usize),
    cells: (usize, usize),
    ground: [u8; 3],
}

/// `rows` as a card on a page of `cpu`'s theme, as the window lays it: its cells with the
/// pane's ground in its corner cells, its glass marks drawn as the chrome's icons, its
/// occluder, and its pixel layers ([`card_layers`]), the wash under `washed`. A card longer
/// than the page is scrolled to its head.
fn shoot(
    cpu: &mut aterm_render::Renderer,
    theme: Theme,
    palette: Option<CardPalette>,
    serious: bool,
    rows: &[MenuRow],
    washed: Option<Washed>,
) -> Shot {
    const PAGE: (usize, usize) = (28, 64);
    let w = (content_width(rows) + CHROME_W).min(PAGE.1 - 8);
    let rect = MenuRect {
        row: 2,
        col: 4,
        w,
        h: (rows.len() + 2).min(PAGE.0 - 6),
    };
    let grid = (rect.row + rect.h + 3, PAGE.1);
    let mut term = aterm_core::terminal::Terminal::new(grid.0 as u16, grid.1 as u16);
    let line = "the page under the card  ".repeat(3);
    let page: String = (1..=grid.0)
        .map(|r| format!("\x1b[{r};1H{}", &line[..grid.1]))
        .collect();
    term.process(format!("\x1b[?25l{page}").as_bytes());
    let mut input = term.cell_frame(grid.0, grid.1);
    // The page in the theme's own colours, so a blank of it is the pane's ground.
    let ground = crate::tab_menu::ground_cell(theme, input.default_bg);
    for cell in input.cells.iter_mut().flatten() {
        (cell.fg, cell.bg) = (CardInks::of(theme, None).quiet(), ground.bg);
    }
    let viewport = MenuViewport::default();
    let mut painted = paint(rect, rows, viewport, washed.map(|w| w.0), theme, palette);
    for row in [0, rect.h - 1] {
        for col in [0, rect.w - 1] {
            painted[row][col] = ground;
        }
    }
    for (r, cells) in painted.iter().enumerate() {
        input.cells[rect.row + r][rect.col..rect.col + rect.w].copy_from_slice(cells);
    }
    let cell = cpu.cell_size();
    let card =
        crate::app_render::scrub_overlay_card(&mut input, rect, cell, (cpu.pad(), cpu.grid_top()))
            .expect("a card");
    let mut marks = crate::claude_card::WindowCards::default();
    for (r, cells) in painted.iter().enumerate() {
        marks.paint_marks(&mut input, rect.row + r, rect.col, cells);
    }
    let inks = CardInks::of(theme, palette);
    let key = plate_key(rect, &inks, cell, 1.0, serious);
    let layers = card_layers(
        (rect, rows, viewport),
        (&inks, key),
        Some(card),
        (9, 10),
        (&mut CardPlate::default(), &mut CardWash::default()),
        washed.map(|washed| (washed, inks.wash.1)),
    );
    assert!(layers[0].is_some(), "every card has a frame");
    assert_eq!(
        layers[1].is_some(),
        washed.is_some(),
        "a wash where a row is lit"
    );
    input.fx_layers.extend(layers.into_iter().flatten());
    let frame = cpu.render_input(&input);
    Shot {
        png: frame.to_png(),
        width: frame.width,
        pixels: frame.pixels.clone(),
        card,
        grid,
        cells: (rect.w, rect.h),
        ground: inks.ground,
    }
}

impl Shot {
    /// The pixel `(dx, dy)` from the card's top left corner, as `0x00RRGGBB`.
    fn at(&self, dx: i32, dy: i32) -> u32 {
        let (x, y) = (self.card.x0 + dx, self.card.y0 + dy);
        let i = usize::try_from(y).expect("on the frame") * self.width
            + usize::try_from(x).expect("on the frame");
        self.pixels[i] & 0x00FF_FFFF
    }
}

/// THE FOOTER CARD GALLERY: every card kind (the model's choices, a choice that cannot be
/// made, the effort's slider, the folder's, the branch's and the context's cards, the
/// account's as it opens and with every kind of row it lays) on Default, Default Light and Solarized Dark, under Cruise, Land, Off and no
/// palette, at rest and lit (the pointer and the keys paint one state,
/// `the_keys_and_the_pointer_paint_one_wash_and_a_hover_builds_no_art`); the permission box
/// under each thing it tells, on a dark and on a light theme; and two cards in Serious Mode and
/// under an OS-forced palette. Each is one CPU-rendered frame (`{kind}-{theme}-{palette}-
/// {state}.png`) listed in `manifest.tsv`, written to `ATERM_FOOTER_CARD_GALLERY` (a scratch
/// folder where unset). Every frame is checked for the one look: the frame's line on the card's
/// top edge is not the card's ground, a blank of the card's body and of its first row are the
/// card's ground, and a lit frame differs from the frame at rest inside the card alone.
#[test]
fn footer_card_gallery() {
    let (dir, _gallery) =
        crate::scripted_redraw_fixture::gallery_directory("ATERM_FOOTER_CARD_GALLERY");
    let mut manifest = String::from(
        "# Synthetic cards on the CPU renderer; no provider, account or credentials. `lit` is \
         the row under the pointer or the keys.\n\
         kind\ttheme\tpalette\tvariant\tstate\tcard_cells\tframe_cells\tfile\n",
    );
    let palettes = [
        ("cruise", Some(CardPalette::Cruise)),
        ("land", Some(CardPalette::Land)),
        ("off", Some(CardPalette::Off)),
        ("none", None),
    ];
    let cards = cards();
    let mut frames = 0usize;
    let mut shot = |cpu: &mut aterm_render::Renderer,
                    (kind, rows): &(String, Vec<MenuRow>),
                    (theme_name, theme): (&str, Theme),
                    (palette_name, palette): (&str, Option<CardPalette>),
                    variant: &str| {
        let serious = variant == "serious";
        let mut rows = rows.clone();
        let rest = shoot(cpu, theme, palette, serious, &rows, None);
        let washed = lit(&mut rows);
        let lit = washed.map(|washed| shoot(cpu, theme, palette, serious, &rows, Some(washed)));
        let what = format!("{kind} {theme_name} {palette_name} {variant}");
        let (cw, ch) = cpu.cell_size();
        let (cw, ch) = (cw as i32, ch as i32);
        for (state, s) in [("rest", Some(&rest)), ("lit", lit.as_ref())] {
            let Some(s) = s else {
                continue;
            };
            let ground = u32::from_be_bytes([0, s.ground[0], s.ground[1], s.ground[2]]);
            let wide = s.cells.0 as i32 * cw;
            assert_ne!(s.at(wide / 2, 0), ground, "{what} {state}: the line");
            // The card's first row and its body, one cell in from the right edge: blank on
            // every card (a scroll cue sits one cell further in).
            assert_eq!(s.at(wide - cw - cw / 2, ch / 2), ground, "{what} {state}");
            assert_eq!(
                s.at(wide - cw / 4, ch + ch / 2),
                ground,
                "{what} {state}: the body beside the line"
            );
            let stem = format!(
                "{kind}-{}-{palette_name}{}-{state}",
                theme_name.to_lowercase().replace(' ', "-"),
                if variant == "plain" {
                    String::new()
                } else {
                    format!("-{variant}")
                }
            );
            let file = format!("{stem}.png");
            std::fs::write(dir.join(&file), &s.png).expect("a gallery frame");
            manifest.push_str(&format!(
                "{kind}\t{theme_name}\t{palette_name}\t{variant}\t{state}\t{}x{}\t{}x{}\t{file}\n",
                s.cells.0, s.cells.1, s.grid.1, s.grid.0
            ));
            frames += 1;
        }
        if let Some(lit) = &lit {
            let inside = |i: usize| {
                let (x, y) = ((i % rest.width) as i32, (i / rest.width) as i32);
                (rest.card.x0..rest.card.x1).contains(&x)
                    && (rest.card.y0..rest.card.y1).contains(&y)
            };
            let moved: Vec<usize> = (0..rest.pixels.len())
                .filter(|&i| rest.pixels[i] != lit.pixels[i])
                .collect();
            assert!(!moved.is_empty(), "{what}: a lit row reads apart");
            assert!(
                moved.into_iter().all(inside),
                "{what}: lit, nothing moves outside the card"
            );
        }
    };
    for theme_name in ["Default", "Default Light", "Solarized Dark"] {
        let scheme = aterm_types::scheme::builtin(theme_name).expect("a built-in theme");
        let theme = Theme::from(&scheme);
        let mut cpu = bundled_cpu_in(theme);
        for palette in palettes {
            for card in cards
                .iter()
                .filter(|(kind, _)| !kind.starts_with("permission-"))
            {
                shot(&mut cpu, card, (theme_name, theme), palette, "plain");
            }
        }
        // The permission box under each thing it tells: its words change, not its look.
        if theme_name != "Solarized Dark" {
            let palette = if theme_name == "Default" {
                palettes[0]
            } else {
                palettes[3]
            };
            for card in cards
                .iter()
                .filter(|(kind, _)| kind.starts_with("permission-"))
            {
                shot(&mut cpu, card, (theme_name, theme), palette, "plain");
            }
        }
        if theme_name == "Default" {
            for kind in ["model", "effort"] {
                let card = cards.iter().find(|(k, _)| k == kind).expect("a card");
                shot(&mut cpu, card, (theme_name, theme), palettes[0], "serious");
            }
            // An OS-forced palette: the system's colours, a flat line and no shadow.
            let hc = crate::chrome_band::ForcedChrome {
                window: [0, 0, 0],
                window_text: [255, 255, 255],
                highlight: [26, 235, 255],
                highlight_text: [0, 0, 0],
                btn_face: [0, 0, 0],
            };
            crate::chrome_band::install_forced_chrome(Some(hc));
            let forced = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                for kind in ["model", "effort", "account"] {
                    let card = cards.iter().find(|(k, _)| k == kind).expect("a card");
                    shot(&mut cpu, card, (theme_name, theme), palettes[0], "forced");
                }
            }));
            crate::chrome_band::install_forced_chrome(None);
            if let Err(panic) = forced {
                std::panic::resume_unwind(panic);
            }
        }
    }
    assert_eq!(
        manifest.lines().count(),
        frames + 2,
        "a manifest line for each frame"
    );
    assert!(frames > 150, "{frames} frames");
    std::fs::write(dir.join("manifest.tsv"), manifest).expect("the manifest");
}
