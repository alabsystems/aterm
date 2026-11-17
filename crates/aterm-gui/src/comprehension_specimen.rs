// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE COMPREHENSION MARKS' SPECIMEN (debug builds only): a session card's head rows and
//! history lines, built of the nodes a card draws ([`crate::comprehension_words::mark_node`],
//! Title and Caption text), compiled by [`UiTree::compile`], painted by
//! [`crate::native_ui::CompiledUi::tray_with_chrome`] and rasterised by
//! [`crate::tray_raster::rasterize_tray`], the CPU path every native page takes. It opens no
//! window and starts no instance. `targo --unverified lab comprehension-gallery` runs it once
//! per look and scale and judges what it writes.
//!
//! It writes `card.<look>.<scale>x.png` and `card.<look>.<scale>x.layout.txt`: an
//! `inspection-source` line with the frame's scale and viewport, then the compiled page's
//! paint audit, in the lines `aterm ctl inspect app/v1 view <view> audit` prints. Its content is
//! invented lab content.

use std::path::{Path, PathBuf};

use aterm_db::words::{Track, TrackWhy};

use crate::comprehension_words::{
    MARK_CAPTION, MARK_TITLE, RowWords, Word, activity_here, mark_node,
};
use crate::native_ui::{
    GroupSpec, Insets, Layout, Length, LogicalRect, SemanticRole, StyleRef, TextSpec, UiContent,
    UiNode, UiTree,
};

/// The card's width, pt.
const CARD_W: f32 = 480.0;
/// The page's and the card's padding, pt.
const PAD: f32 = 16.0;
/// A head row's height, and a history line's.
const ROW_H: f32 = 32.0;
const LINE_H: f32 = 20.0;
/// The widths of a head row's activity word and its age, and a history line's time.
const WORD_W: f32 = 48.0;
const AGE_W: f32 = 28.0;
const TIME_W: f32 = 36.0;

/// One head row of the specimen: what its survey row says, the track rule's verdict, its topic
/// and how long its activity has stood.
struct HeadRow {
    row: RowWords<'static>,
    verdict: Option<TrackWhy>,
    topic: &'static str,
    age: &'static str,
}

/// The specimen's six sessions' rows and verdicts, each the row an activity word is read from:
/// an idle agent an approval blocks (`stuck`), an agent at its weekly wall (`quiet`), an ended
/// one (`ended`), one at work (`working`), one at rest with nothing to judge (`idle`), and a
/// shell, whose activity is unread. The verdict's track word is the row's track mark; the shell
/// has none read.
pub(crate) fn rows() -> [(RowWords<'static>, Option<TrackWhy>); 6] {
    let agent = |agent, needs| RowWords {
        state: Some("alive"),
        program_kind: Some("agent"),
        agent: Some(agent),
        needs: Some(needs),
        ..RowWords::default()
    };
    [
        (agent("idle", "approval"), Some(TrackWhy::Blocked)),
        (agent("wall:usage-weekly", "wall"), Some(TrackWhy::Walled)),
        (
            RowWords {
                state: Some("exited"),
                task: Some("done"),
                ..agent("busy", "-")
            },
            Some(TrackWhy::Landed),
        ),
        (
            RowWords {
                task: Some("working"),
                ..agent("busy", "-")
            },
            Some(TrackWhy::Progress),
        ),
        (agent("idle", "-"), Some(TrackWhy::Untracked)),
        (
            RowWords {
                state: Some("alive"),
                program_kind: Some("shell"),
                needs: Some("-"),
                ..RowWords::default()
            },
            None,
        ),
    ]
}

fn head_rows() -> Vec<HeadRow> {
    let said = [
        ("Review the fold laws", "12m"),
        ("Measure the cold build", "31m"),
        ("Land the updater fix", "1h"),
        ("Fix the stale-lock retry", "4m"),
        ("Read the release notes", "2h"),
        ("A shell on m18", ""),
    ];
    rows()
        .into_iter()
        .zip(said)
        .map(|((row, verdict), (topic, age))| HeadRow {
            row,
            verdict,
            topic,
            age,
        })
        .collect()
}

fn group(key: impl Into<std::sync::Arc<str>>) -> UiNode {
    UiNode::new(
        key,
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
}

/// `node`, `slot` square, sat `top` down a column `band` tall.
fn seat(key: String, node: UiNode, slot: f32, band: f32) -> UiNode {
    group(key)
        .layout(
            Layout::column()
                .width(Length::Fixed(slot))
                .height(Length::Fixed(band))
                .padding(Insets {
                    top: (band - slot) / 2.0,
                    ..Insets::default()
                }),
        )
        .children(vec![node])
        .paint_only()
}

fn words(key: String, text: &str, role: SemanticRole, style: StyleRef, width: Length) -> UiNode {
    UiNode::new(
        key,
        UiContent::Text(TextSpec {
            text: text.to_owned(),
            role,
            style,
        }),
    )
    .layout(Layout::default().width(width))
}

/// One head row: the track mark, the topic as the row's Title, and at its right edge the activity
/// icon, its word (paint only: the icon speaks it) and its age. A row whose activity is unread
/// draws no word and no age, in slots of the same widths.
fn head_row(i: usize, row: &HeadRow) -> UiNode {
    let key = |part: &str| format!("specimen/r{i}/{part}");
    let track = Word::Track(row.verdict.map(TrackWhy::track));
    let activity = activity_here(&row.row, row.verdict);
    let mut children = vec![
        seat(
            key("seat"),
            mark_node(key("mark"), track, MARK_TITLE),
            MARK_TITLE,
            ROW_H,
        ),
        words(
            key("topic"),
            row.topic,
            SemanticRole::Heading,
            StyleRef::Primary,
            Length::Fill,
        ),
        seat(
            key("icon-seat"),
            mark_node(key("icon"), Word::Activity(activity), MARK_TITLE),
            MARK_TITLE,
            ROW_H,
        ),
    ];
    match activity {
        Some(activity) => children.extend([
            words(
                key("word"),
                activity.word(),
                SemanticRole::Text,
                StyleRef::Note,
                Length::Fixed(WORD_W),
            )
            .paint_only(),
            words(
                key("time"),
                row.age,
                SemanticRole::Text,
                StyleRef::Note,
                Length::Fixed(AGE_W),
            ),
        ]),
        None => children.extend([
            group(key("word")).layout(Layout::default().width(Length::Fixed(WORD_W))),
            group(key("time")).layout(Layout::default().width(Length::Fixed(AGE_W))),
        ]),
    }
    group(key("row"))
        .layout(Layout::row().gap(8.0).height(Length::Fixed(ROW_H)))
        .children(children)
}

/// One history line: the event mark where aterm's own writer reported the line (a silent slot
/// of the same size where it did not), its time and its words.
fn history_line(i: usize, event: Option<&'static str>, time: &str, line: &str) -> UiNode {
    let key = |part: &str| format!("specimen/h{i}/{part}");
    let mark = match event {
        Some(event) => Word::Event(event),
        None => Word::Track(Some(Track::Untracked)),
    };
    group(key("row"))
        .layout(Layout::row().gap(8.0).height(Length::Fixed(LINE_H)))
        .children(vec![
            seat(
                key("seat"),
                mark_node(key("mark"), mark, MARK_CAPTION),
                MARK_CAPTION,
                LINE_H,
            ),
            words(
                key("time"),
                time,
                SemanticRole::Text,
                StyleRef::Note,
                Length::Fixed(TIME_W),
            ),
            words(
                key("line"),
                line,
                SemanticRole::Text,
                StyleRef::Note,
                Length::Fill,
            ),
        ])
}

/// The specimen's page: one card on the page, its six head rows and three history lines, newest
/// first, on the 4 pt grid.
fn page() -> (UiTree, LogicalRect) {
    let heads: Vec<UiNode> = head_rows()
        .iter()
        .enumerate()
        .map(|(i, row)| head_row(i, row))
        .collect();
    let heads_h = ROW_H * 6.0 + 4.0 * 5.0;
    // Newest first, as the card lists its history (§2.2).
    let lines = vec![
        history_line(0, None, "14:58", "Tests pass: 41 of 41"),
        history_line(1, Some("commit"), "14:51", "Committed 9f3c2ab41d07"),
        history_line(2, Some("chapter"), "14:40", "Chapter 2 opened"),
    ];
    let lines_h = LINE_H * 3.0 + 4.0 * 2.0;
    let card_h = PAD * 2.0 + heads_h + 8.0 + lines_h;
    let viewport = LogicalRect::new(0.0, 0.0, CARD_W + PAD * 2.0, card_h + PAD * 2.0);
    let column = |key: &str, gap: f32, height: f32, children: Vec<UiNode>| {
        group(key)
            .layout(
                Layout::column()
                    .gap(gap)
                    .height(Length::Fixed(height))
                    .width(Length::Fill),
            )
            .children(children)
    };
    let card = UiNode::new(
        "specimen/card",
        UiContent::Group(GroupSpec {
            label: Some("Session card".to_owned()),
            role: SemanticRole::Group,
            style: StyleRef::Secondary,
        }),
    )
    .layout(
        Layout::column()
            .gap(8.0)
            .width(Length::Fixed(CARD_W))
            .height(Length::Fixed(card_h))
            .padding(Insets::all(PAD)),
    )
    .children(vec![
        column("specimen/heads", 4.0, heads_h, heads),
        column("specimen/history", 4.0, lines_h, lines),
    ]);
    let root = UiNode::new(
        "specimen",
        UiContent::Group(GroupSpec {
            label: Some("Comprehension marks".to_owned()),
            role: SemanticRole::Application,
            style: StyleRef::Plain,
        }),
    )
    .layout(Layout::column().padding(Insets::all(PAD)))
    .children(vec![card]);
    (UiTree::new(root), viewport)
}

/// A look: a theme and whether High Contrast is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Look {
    Dark,
    Light,
    HcDark,
    HcLight,
}

impl Look {
    fn parse(word: &str) -> Option<Self> {
        Some(match word {
            "dark" => Self::Dark,
            "light" => Self::Light,
            "hc-dark" => Self::HcDark,
            "hc-light" => Self::HcLight,
            _ => return None,
        })
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
            Self::HcDark => "hc-dark",
            Self::HcLight => "hc-light",
        }
    }

    const fn light(self) -> bool {
        matches!(self, Self::Light | Self::HcLight)
    }

    const fn high_contrast(self) -> bool {
        matches!(self, Self::HcDark | Self::HcLight)
    }

    /// Its theme: the default (dark), or the bundled `GitHub Light`.
    fn theme(self) -> Result<aterm_render::Theme, String> {
        if !self.light() {
            return Ok(aterm_render::Theme::default());
        }
        aterm_types::scheme::builtin("GitHub Light")
            .map(|scheme| aterm_render::Theme::from(&scheme))
            .ok_or_else(|| "this build bundles no `GitHub Light` theme".to_owned())
    }
}

const USAGE: &str = "usage: comprehension_specimen --out DIR --look dark|light|hc-dark|hc-light \
                     --scale 1|2";

/// What the arguments ask for: where to write, the look and the scale.
fn parse_args(args: &[String]) -> Result<(PathBuf, Look, u32), String> {
    let (mut out, mut look, mut scale) = (None, None, None);
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let value = it.next().ok_or_else(|| format!("{flag} takes a value"))?;
        match flag.as_str() {
            "--out" => out = Some(PathBuf::from(value)),
            "--look" => {
                look = Some(Look::parse(value).ok_or_else(|| format!("no look {value:?}"))?);
            }
            "--scale" => {
                scale = Some(match value.as_str() {
                    "1" => 1,
                    "2" => 2,
                    _ => return Err(format!("no scale {value:?}")),
                });
            }
            _ => return Err(format!("no flag {flag:?}")),
        }
    }
    Ok((
        out.ok_or("--out is required")?,
        look.ok_or("--look is required")?,
        scale.ok_or("--scale is required")?,
    ))
}

/// Run the specimen with `args` (`--out DIR --look LOOK --scale 1|2`): 0 its frame and layout
/// written, 1 a compile or a write failed, 2 a usage error (an unknown flag or value, or a High
/// Contrast look the process's `ATERM_DEV_ACCESSIBILITY` does not turn on, or the reverse).
#[must_use]
pub fn run(args: &[String]) -> i32 {
    let (out, look, scale) = match parse_args(args) {
        Ok(asked) => asked,
        Err(why) => {
            eprintln!("comprehension_specimen: {why}\n{USAGE}");
            return 2;
        }
    };
    // The development seam's accessibility settings, folded in as an instance folds them.
    let _ = crate::native_appearance::install_preferences(
        crate::native_appearance::AppearancePreferences::default(),
    );
    let high_contrast = crate::native_appearance::current_preferences().high_contrast;
    if high_contrast != look.high_contrast() {
        eprintln!(
            "comprehension_specimen: look {} wants High Contrast {}, and \
             ATERM_DEV_ACCESSIBILITY gives {high_contrast}\n{USAGE}",
            look.name(),
            look.high_contrast()
        );
        return 2;
    }
    match write(&out, look, scale) {
        Ok(()) => 0,
        Err(why) => {
            eprintln!("comprehension_specimen: {why}");
            1
        }
    }
}

/// Render the specimen in `look` at `scale` and write its frame and layout into `out`.
fn write(out: &Path, look: Look, scale: u32) -> Result<(), String> {
    let theme = look.theme()?;
    crate::tray_raster::set_chrome_fonts(None, None, None);
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let (tree, viewport) = page();
    let compiled = tree
        .compile(viewport)
        .map_err(|e| format!("the specimen does not compile: {e:?}"))?;
    let roles = crate::settings::Roles::from_theme(theme);
    let tray = compiled.tray_with_chrome(theme, theme, 13.0, 0);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the viewport is a few hundred points"
    )]
    let (w, h) = (viewport.width as u32, viewport.height as u32);
    #[allow(clippy::cast_precision_loss, reason = "a scale of 1 or 2")]
    let scale_f = scale as f32;
    let backdrop = crate::widget::rgba(roles.surface, 255);
    let (rgba, pw, ph) = crate::tray_raster::rasterize_tray(&tray.prims, w, h, scale_f, backdrop);
    let png =
        crate::app_introspect::encode_srgb_rgba8(pw, ph, |writer| writer.write_image_data(&rgba))?;
    let stem = format!("card.{}.{scale}x", look.name());
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let png_path = out.join(format!("{stem}.png"));
    std::fs::write(&png_path, png).map_err(|e| format!("{}: {e}", png_path.display()))?;
    let mut layout = vec![format!(
        "inspection-source source=specimen view=0 window=0 generation=0 geometry={:016x} \
         scale={scale_f:.3} viewport={:.1},{:.1},{:.1},{:.1} config-revision=0 \
         update-revision=0 document-seq=- presentation-revision=0 paint-revision={:016x} \
         model-current=true capture-serial=0 compiled-fingerprint={:016x}",
        0,
        viewport.x,
        viewport.y,
        viewport.width,
        viewport.height,
        0,
        compiled.fingerprint(),
    )];
    layout.extend(compiled.paint_audit_lines());
    let layout_path = out.join(format!("{stem}.layout.txt"));
    std::fs::write(&layout_path, layout.join("\n") + "\n")
        .map_err(|e| format!("{}: {e}", layout_path.display()))?;
    Ok(())
}
