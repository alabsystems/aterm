// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `comprehension-gallery`: the comprehension marks (`docs/DESIGN-comprehension-2026-11-14.md`
//! §5.6, §5.7) laid out as a session card draws them, rendered offscreen by aterm-gui's debug
//! specimen (`crates/aterm-gui/src/comprehension_specimen.rs`) through the native page raster,
//! one frame per look and scale, each in a scratch world ([`crate::instance::World`]'s
//! environment, as an instance runs). No window opens and no instance starts. Each frame is
//! judged by [`crate::system_state_measure`]'s checks and four of its own: one column x across
//! track words, the unread cell drawn `?`, every form distinct in grey, and the event mark
//! hollow.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::instance::{World, take_flag, take_value, take_values};
use crate::paths::{from_caller, paths};
use crate::pyjson::J;
use crate::system_state_gallery::Look;
use crate::system_state_measure::{self as measure, Check, Finding, Layout, Node, Rect};
use crate::trail_contrast::{Image, contrast, round_to};
use crate::trail_gallery::{checkouts, resolve_out};
use crate::{Fail, Res};

/// The verb's own help (`comprehension-gallery --help`).
pub const HELP: &str = "\
usage: targo --unverified lab comprehension-gallery [--view card|navigator] [--looks LOOK[,LOOK]]...
         [--scales 1|2[,1|2]] [--specimen PATH] [--keep] <out_dir>

Render the comprehension marks (track marks, activity icons, the event mark and the unread ?) as a session card
lays them out, offscreen through the native page raster: six head rows (the track mark, the topic, the activity icon,
its word and its age) and three history lines, one frame per look and scale. Each frame is the debug specimen's run in
a scratch world (a scratch HOME and XDG dirs, as an instance runs); no window opens and no instance starts. Build the
specimen first: targo --unverified build -p aterm-gui --example comprehension_specimen

  <out_dir>      where the frames and the report go, OUTSIDE the repository; made if missing
  --view         card (the default); navigator is not run (exit 3): no navigator view places these marks yet
  --looks        dark (the default theme), light (\"GitHub Light\"), hc-dark, hc-light (High Contrast, from the
                 specimen's ATERM_DEV_ACCESSIBILITY seam); high-contrast names both of its; all (the default)
  --scales       1, 2 or both (the default)
  --specimen     the specimen to run (default: examples/comprehension_specimen beside this lab's executable)
  --keep         retain every run's scratch world

Each frame is card.<look>.<scale>x: .png and .layout.txt (the compiled page's paint audit); report.txt and report.json.
Where the marks and words are is read from the layout; their colours from the PNG:

  text     every node whose words the layout audits >= 4.5:1 on its background
  glyph    every mark (a custom node) >= 3:1 on its wash and on the card it sits on
  card     the card >= 1.2:1 from the page
  clip     no words ellipsised or cut
  rhythm   every visible node's top and height on the 4 pt grid
  columns  every head row's topic at one x and its activity icon at one right edge, the row with no track included
  unread   the unread row's mark and icon say `track unread` and `activity unread`, each inks something, and no word of
           that row is a painted 0
  grey     each mark's pixels that stand >= 1.5:1 from its ground: two marks of different forms differ in at least
           25% of their inked union, the larger resampled to the smaller's pixels where their sizes differ; two of one
           form and one size (the unread pair, the waiting form quiet and stuck share, the event marks) agree; a mark
           compared with no mark of another form fails
  hollow   each event mark (a history line's) encloses at least 3 pt^2 of pixels under 1.5:1 from its ground, so its
           outlined triangle never reads as working's filled one

exit: 0 every frame taken and every check passed; 1 a check failed or a frame was not taken; 2 usage; 3 not run (the
navigator view, no specimen, or an <out_dir> that cannot be made)
";

/// The specimen's build command, named where it is missing.
const BUILD: &str = "targo --unverified build -p aterm-gui --example comprehension_specimen";

/// A pixel inks a mark where it stands at least this far from the wash the mark sits on.
const INKED: f64 = 1.5;
/// Two marks of different forms differ in at least this share of their inked union.
const APART_MIN: f64 = 0.25;
/// Two marks of one form differ in at most this share of it.
const ALIKE_MAX: f64 = 0.05;
/// An event mark encloses at least this many square points its outline does not ink.
const HOLLOW_MIN: f64 = 3.0;
/// A column's x, or its right edge, agrees within this many points.
const COLUMN_SLACK: f64 = 0.05;

/// What the arguments ask for.
#[derive(Debug, PartialEq)]
struct Args {
    looks: Vec<Look>,
    scales: Vec<u32>,
    specimen: Option<PathBuf>,
    keep: bool,
    out: String,
}

fn usage(msg: impl AsRef<str>) -> Fail {
    Fail::usage(format!("comprehension-gallery: {}", msg.as_ref()))
}

/// Every comma-separated word of a repeatable flag's values.
fn words(values: &[String]) -> Vec<String> {
    values
        .iter()
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// The looks a `--looks` word names.
fn looks_of(word: &str) -> Option<Vec<Look>> {
    Some(match word {
        "dark" => vec![Look::Dark],
        "light" => vec![Look::Light],
        "hc-dark" => vec![Look::HcDark],
        "hc-light" => vec![Look::HcLight],
        "high-contrast" => vec![Look::HcDark, Look::HcLight],
        "all" => vec![Look::Dark, Look::Light, Look::HcDark, Look::HcLight],
        _ => return None,
    })
}

/// Read the arguments. The navigator view is refused as not run (exit 3), after every flag is
/// read, so a usage error is still one.
fn parse_args(args: &[String]) -> Result<Args, Fail> {
    let mut args = args.to_vec();
    let view = take_value(&mut args, "--view")?;
    let looks = words(&take_values(&mut args, "--looks")?);
    let scales = words(&take_values(&mut args, "--scales")?);
    let specimen = take_value(&mut args, "--specimen")?.map(|p| PathBuf::from(from_caller(&p)));
    let keep = take_flag(&mut args, "--keep");
    let looks = if looks.is_empty() {
        vec!["all".to_string()]
    } else {
        looks
    };
    let scales = if scales.is_empty() {
        vec!["1".to_string(), "2".to_string()]
    } else {
        scales
    };
    let mut chosen = Vec::new();
    for word in looks {
        for look in looks_of(&word).ok_or_else(|| usage(format!("no look {word:?}")))? {
            if !chosen.contains(&look) {
                chosen.push(look);
            }
        }
    }
    let mut at = Vec::new();
    for word in scales {
        let scale = match word.as_str() {
            "1" => 1,
            "2" => 2,
            _ => return Err(usage(format!("no scale {word:?}: 1 or 2"))),
        };
        if !at.contains(&scale) {
            at.push(scale);
        }
    }
    if let Some(flag) = args.iter().find(|a| a.starts_with("--")) {
        return Err(usage(format!("no flag {flag}")));
    }
    let [out] = args.as_slice() else {
        return Err(usage("one <out_dir> is required"));
    };
    match view.as_deref() {
        None | Some("card") => {}
        Some("navigator") => {
            return Err(Fail::exit(
                3,
                "comprehension-gallery: not run: this build has no navigator view to photograph",
            ));
        }
        Some(other) => return Err(usage(format!("no view {other:?}: card or navigator"))),
    }
    Ok(Args {
        looks: chosen,
        scales: at,
        specimen,
        keep,
        out: out.clone(),
    })
}

/// A frame's name.
fn frame_name(look: Look, scale: u32) -> String {
    format!("card.{}.{scale}x", look.name())
}

/// The specimen beside this lab's own executable.
fn default_specimen() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot read this lab's path: {e}"))?;
    let dir = exe
        .parent()
        .ok_or_else(|| format!("{} has no directory", exe.display()))?;
    Ok(dir.join("examples").join("comprehension_specimen"))
}

/// Run the specimen once for `look` at `scale` in a scratch world.
fn take(specimen: &Path, out: &Path, look: Look, scale: u32, keep: bool) -> Result<(), String> {
    let mut world = World::new("")?;
    if keep {
        world.keep();
    }
    let mut env = world.env();
    if let Some(word) = look.accessibility() {
        env.push(("ATERM_DEV_ACCESSIBILITY".into(), word.into()));
    }
    let mut cmd = aterm_uds::spawnfd::command(specimen);
    cmd.env_clear().envs(env).arg("--out").arg(out).args([
        "--look",
        look.name(),
        "--scale",
        &scale.to_string(),
    ]);
    let output = aterm_uds::deadline::output_while_process_progressing(
        &mut cmd,
        Duration::from_secs(60),
        "the comprehension specimen",
    )
    .map_err(|e| e.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "the specimen exited {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

/// The run: one specimen per look and scale, then the report.
/// # Errors
/// A usage error, the navigator view, a missing specimen, or an out dir that cannot be made.
pub fn cmd(args: &[String]) -> Res {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return Ok(0);
    }
    let a = parse_args(args)?;
    let out = resolve_out(&from_caller(&a.out), &checkouts(&paths().repo)).map_err(usage)?;
    let specimen = match a.specimen.clone() {
        Some(path) => path,
        None => default_specimen().map_err(|e| Fail::exit(3, e))?,
    };
    if !specimen.is_file() {
        return Err(Fail::exit(
            3,
            format!(
                "comprehension-gallery: not run: no specimen at {}; build it with `{BUILD}`",
                specimen.display()
            ),
        ));
    }
    std::fs::create_dir_all(&out).map_err(|e| {
        Fail::exit(
            3,
            format!("comprehension-gallery: cannot make {}: {e}", out.display()),
        )
    })?;
    let mut taken = Vec::new();
    let mut missed = Vec::new();
    for &look in &a.looks {
        for &scale in &a.scales {
            let name = frame_name(look, scale);
            match take(&specimen, &out, look, scale, a.keep) {
                Ok(()) => taken.push(name),
                Err(why) => missed.push((name, why)),
            }
        }
    }
    let passed = report(&out, &taken, &missed, &specimen)
        .map_err(|e| Fail::die(format!("comprehension-gallery: {e}")))?;
    println!(
        "comprehension-gallery: {} frame(s) -> {}",
        taken.len(),
        out.display()
    );
    Ok(i32::from(!passed))
}

/// A check of this gallery's own, by name, with nothing judged yet.
const fn own(name: &'static str) -> Check {
    Check {
        name,
        judged: 0,
        worst: None,
        failures: Vec::new(),
        skipped: Vec::new(),
    }
}

fn fail(check: &mut Check, key: &str, value: f64, detail: String) {
    check.failures.push(Finding {
        key: key.to_string(),
        value: round_to(value, 2),
        detail,
    });
}

/// The head row a node's key names, and its part: `specimen/r3/topic` is `(3, "topic")`.
fn head_part(key: &str) -> Option<(usize, &str)> {
    let (row, part) = key.strip_prefix("specimen/r")?.split_once('/')?;
    Some((row.parse().ok()?, part))
}

/// COLUMNS: every head row's topic at one x and its activity icon at one right edge.
fn columns_check(layout: &Layout) -> Check {
    let mut check = own("columns");
    let mut worst: f64 = 0.0;
    let edges: [(&str, fn(Rect) -> f64); 2] = [("topic", |r| r.x), ("icon", Rect::right)];
    for (part, edge) in edges {
        let nodes: Vec<&Node> = layout
            .nodes
            .iter()
            .filter(|n| head_part(&n.key).is_some_and(|(_, p)| p == part))
            .collect();
        let Some(first) = nodes.first() else {
            fail(&mut check, part, 0.0, format!("no head row has a {part}"));
            continue;
        };
        let at = edge(first.rect);
        for node in &nodes {
            check.judged += 1;
            let off = (edge(node.rect) - at).abs();
            worst = worst.max(off);
            if off > COLUMN_SLACK {
                let detail = format!("{part} at {:.2} pt, the first at {at:.2}", edge(node.rect));
                fail(&mut check, &node.key, off, detail);
            }
        }
    }
    check.worst = Some(round_to(worst, 2));
    check
}

/// Where a mark inks its rect at the layout's scale: each pixel at least [`INKED`] from the wash
/// it sits on ([`measure::wash`], the glyph check's), row-major, and the mask's width (the rect's
/// pixel columns inside the frame); `None` where no wash can be read or the mark paints nothing
/// on it (the glyph check's `blank`).
fn mask_of(frame: &Image, layout: &Layout, rect: Rect) -> Option<(Vec<bool>, i64)> {
    let inner = rect.px(layout.scale);
    let wash = measure::wash(frame, inner, layout.viewport.px(layout.scale))?;
    let px = frame.region(inner);
    measure::solid_ink(&px, wash)?;
    let mask = px.into_iter().map(|p| contrast(p, wash) >= INKED).collect();
    let column = |v: i64| v.clamp(0, i64::try_from(frame.width).unwrap_or(i64::MAX));
    Some((mask, column(inner.x1) - column(inner.x0)))
}

/// A mask `w` pixels wide sampled onto a grid `to_w` wide and `to_h` tall, each pixel taking the
/// one under its centre: `(x, y)` takes `((2x + 1) w / 2 to_w, (2y + 1) h / 2 to_h)`. A form is
/// centred in its slot with a radius of half the slot, so one form at two sizes is the same shape
/// on two grids, but for its outline, which keeps its width.
fn resample(mask: &[bool], w: i64, to_w: i64, to_h: i64) -> Vec<bool> {
    let h = rows(mask, w);
    let mut out = Vec::new();
    for y in 0..to_h {
        for x in 0..to_w {
            let (sx, sy) = (
                ((2 * x + 1) * w) / (2 * to_w),
                ((2 * y + 1) * h) / (2 * to_h),
            );
            out.push(usize::try_from(sy * w + sx).is_ok_and(|i| mask.get(i) == Some(&true)));
        }
    }
    out
}

/// The rows of a mask `w` pixels wide.
fn rows(mask: &[bool], w: i64) -> i64 {
    i64::try_from(mask.len()).unwrap_or(i64::MAX) / w.max(1)
}

/// The share of two masks' inked union where they disagree.
fn apart(a: &[bool], b: &[bool]) -> f64 {
    let union = a.iter().zip(b).filter(|(a, b)| **a || **b).count();
    let differ = a.iter().zip(b).filter(|(a, b)| a != b).count();
    differ as f64 / union.max(1) as f64
}

/// The form a mark is drawn in, for the grey judge: the two unread marks share one, `quiet` and
/// `stuck` share the waiting form, and every history line's mark is the event mark; any other
/// mark's form is its word's own.
fn form_of(node: &Node) -> &str {
    match node.label.as_str() {
        _ if node.key.starts_with("specimen/h") => "event",
        "track unread" | "activity unread" => "unread",
        "quiet" | "stuck" => "waiting",
        label => label,
    }
}

/// The visible custom nodes that speak a word: the marks.
fn marks(layout: &Layout) -> impl Iterator<Item = &Node> {
    layout
        .nodes
        .iter()
        .filter(|n| n.visible && n.kind == "custom" && !n.label.is_empty())
}

/// GREY: every two marks of different forms differ in at least [`APART_MIN`] of their inked
/// union, the larger resampled onto the smaller's pixels where their sizes differ, and two of one
/// form and one size in at most [`ALIKE_MAX`] (one form at two sizes is not judged: its outline
/// keeps its width, so the two are not pixel-alike). A mark is a visible custom node that speaks a
/// word; one that inks nothing fails, as does one compared with no mark of another form, and a
/// frame in which no two marks were judged.
fn grey_check(frame: &Image, layout: &Layout) -> Check {
    let mut check = own("grey");
    let mut masks: Vec<(&Node, Vec<bool>, i64)> = Vec::new();
    for node in marks(layout) {
        match mask_of(frame, layout, node.rect) {
            Some((mask, width)) => masks.push((node, mask, width)),
            None => fail(
                &mut check,
                &node.key,
                0.0,
                format!("{:?} inks nothing", node.label),
            ),
        }
    }
    let mut worst: Option<f64> = None;
    let mut met = vec![false; masks.len()];
    for (i, (a, mask_a, width_a)) in masks.iter().enumerate() {
        for (j, (b, mask_b, width_b)) in masks.iter().enumerate().skip(i + 1) {
            let one_size = width_a == width_b && mask_a.len() == mask_b.len();
            let one_form = form_of(a) == form_of(b);
            if one_form && !one_size {
                continue;
            }
            let share = if one_size {
                apart(mask_a, mask_b)
            } else if mask_a.len() > mask_b.len() {
                let onto = resample(mask_a, *width_a, *width_b, rows(mask_b, *width_b));
                apart(&onto, mask_b)
            } else {
                let onto = resample(mask_b, *width_b, *width_a, rows(mask_a, *width_a));
                apart(mask_a, &onto)
            };
            check.judged += 1;
            let pair = format!(
                "{:?} and {:?} differ in {:.0}%",
                a.label,
                b.label,
                share * 100.0
            );
            if one_form {
                if share > ALIKE_MAX {
                    fail(
                        &mut check,
                        &a.key,
                        share,
                        format!("{pair}, drawn in one form"),
                    );
                }
            } else {
                (met[i], met[j]) = (true, true);
                worst = Some(worst.map_or(share, |w| w.min(share)));
                if share < APART_MIN {
                    fail(&mut check, &a.key, share, pair);
                }
            }
        }
    }
    for ((node, ..), met) in masks.iter().zip(met) {
        if !met {
            let detail = format!("{:?} compared with no mark of another form", node.label);
            fail(&mut check, &node.key, 0.0, detail);
        }
    }
    if check.judged == 0 {
        fail(&mut check, "-", 0.0, "no two marks judged".to_string());
    }
    check.worst = worst.map(|w| round_to(w, 2));
    check
}

/// HOLLOW: every event mark encloses at least [`HOLLOW_MIN`] square points its outline leaves
/// un-inked (by [`mask_of`]'s rule): the un-inked pixels no path of un-inked pixels joins to its
/// rect's edge. Its worst is the smallest hollow, in square points; a frame with no event mark
/// fails, so the check never passes on nothing.
fn hollow_check(frame: &Image, layout: &Layout) -> Check {
    let mut check = own("hollow");
    let mut worst: Option<f64> = None;
    let px_per_pt2 = layout.scale * layout.scale;
    for node in marks(layout).filter(|n| form_of(n) == "event") {
        check.judged += 1;
        let Some((mask, width)) = mask_of(frame, layout, node.rect) else {
            fail(&mut check, &node.key, 0.0, "blank".to_string());
            continue;
        };
        let hollow = enclosed(&mask, width) as f64 / px_per_pt2;
        worst = Some(worst.map_or(hollow, |w| w.min(hollow)));
        if hollow < HOLLOW_MIN {
            let detail = format!(
                "{:?} encloses {hollow:.2} pt^2 un-inked, under {HOLLOW_MIN}",
                node.label
            );
            fail(&mut check, &node.key, hollow, detail);
        }
    }
    if check.judged == 0 {
        fail(&mut check, "-", 0.0, "no event mark".to_string());
    }
    check.worst = worst.map(|w| round_to(w, 2));
    check
}

/// How many un-inked pixels of a mask `w` pixels wide no path of un-inked pixels (each step to a
/// pixel beside, above or below) joins to the mask's edge.
fn enclosed(mask: &[bool], w: i64) -> usize {
    let (Ok(w), Ok(h)) = (usize::try_from(w), usize::try_from(rows(mask, w))) else {
        return 0;
    };
    if w == 0 || h == 0 {
        return 0;
    }
    let mut reached = vec![false; w * h];
    let mut todo: Vec<(usize, usize)> = (0..w)
        .flat_map(|x| [(x, 0), (x, h - 1)])
        .chain((0..h).flat_map(|y| [(0, y), (w - 1, y)]))
        .collect();
    while let Some((x, y)) = todo.pop() {
        let i = y * w + x;
        if reached[i] || mask[i] {
            continue;
        }
        reached[i] = true;
        if x > 0 {
            todo.push((x - 1, y));
        }
        if x + 1 < w {
            todo.push((x + 1, y));
        }
        if y > 0 {
            todo.push((x, y - 1));
        }
        if y + 1 < h {
            todo.push((x, y + 1));
        }
    }
    (0..w * h).filter(|&i| !mask[i] && !reached[i]).count()
}

/// UNREAD: the unread row's mark and icon say `track unread` and `activity unread` and each inks
/// something, and no word of that row is a painted `0`.
fn unread_check(frame: &Image, layout: &Layout) -> Check {
    let mut check = own("unread");
    let Some((row, _)) = layout
        .nodes
        .iter()
        .find(|n| n.label == "track unread")
        .and_then(|n| head_part(&n.key))
    else {
        fail(
            &mut check,
            "-",
            0.0,
            "no mark says `track unread`".to_string(),
        );
        return check;
    };
    for (part, said) in [("mark", "track unread"), ("icon", "activity unread")] {
        let key = format!("specimen/r{row}/{part}");
        check.judged += 1;
        let Some(node) = layout.nodes.iter().find(|n| n.key == key) else {
            fail(&mut check, &key, 0.0, "not laid out".to_string());
            continue;
        };
        if node.label != said {
            fail(
                &mut check,
                &key,
                0.0,
                format!("says {:?}, not {said:?}", node.label),
            );
        }
        if mask_of(frame, layout, node.rect).is_none() {
            fail(&mut check, &key, 0.0, "blank".to_string());
        }
    }
    let prefix = format!("specimen/r{row}/");
    for text in layout.texts.iter().filter(|t| t.key.starts_with(&prefix)) {
        check.judged += 1;
        if text.painted.trim() == "0" {
            fail(&mut check, &text.key, 0.0, "paints 0".to_string());
        }
    }
    check
}

/// Every check of one frame, in the report's order.
fn judge(frame: &Image, layout: &Layout) -> Vec<Check> {
    let mut checks = measure::measure(frame, layout);
    checks.push(columns_check(layout));
    checks.push(unread_check(frame, layout));
    checks.push(grey_check(frame, layout));
    checks.push(hollow_check(frame, layout));
    checks
}

/// Judge the frames, write report.txt and report.json, and say whether every frame was taken and
/// passed.
fn report(
    out: &Path,
    names: &[String],
    missed: &[(String, String)],
    specimen: &Path,
) -> Result<bool, String> {
    let mut text = vec![format!("comprehension-gallery: {}", specimen.display())];
    let mut frames = Vec::new();
    let mut not_taken: Vec<(String, String)> = missed.to_vec();
    let mut passed = 0;
    for name in names {
        let judged = (|| -> Result<Vec<Check>, String> {
            let frame = Image::read(&out.join(format!("{name}.png")))?;
            let layout = crate::util::read_regular_text(&out.join(format!("{name}.layout.txt")))
                .map_err(|e| format!("{name}.layout.txt: {e}"))?;
            let layout = measure::parse_layout(&layout)?;
            Ok(judge(&frame, &layout))
        })();
        let checks = match judged {
            Ok(checks) => checks,
            Err(why) => {
                not_taken.push((name.clone(), why));
                continue;
            }
        };
        let pass = checks.iter().all(Check::pass);
        passed += usize::from(pass);
        let phrases: Vec<String> = checks.iter().map(Check::phrase).collect();
        let line = format!("{name:<18} {}", phrases.join(" | "));
        println!("{line}");
        text.push(line);
        for c in &checks {
            for f in c.failures.iter().take(12) {
                text.push(format!(
                    "    {} {} {}: {}",
                    c.name, f.value, f.key, f.detail
                ));
            }
        }
        frames.push(J::obj([
            ("frame", J::s(name.as_str())),
            ("pass", J::Bool(pass)),
            (
                "checks",
                J::Obj(
                    checks
                        .iter()
                        .map(|c| (c.name.to_string(), c.json()))
                        .collect(),
                ),
            ),
        ]));
    }
    for (name, why) in &not_taken {
        let line = format!("{name}: NOT TAKEN: {why}");
        println!("{line}");
        text.push(line);
    }
    let totals = format!(
        "totals: {} frame(s), {passed} passed, {} not taken",
        frames.len(),
        not_taken.len()
    );
    println!("{totals}");
    text.push(totals.clone());
    crate::util::write_out(&out.join("report.txt"), (text.join("\n") + "\n").as_bytes())
        .map_err(|e| format!("report.txt: {e}"))?;
    let json = J::obj([
        ("specimen", J::s(specimen.display().to_string())),
        (
            "thresholds",
            J::obj([
                ("text", J::Float(measure::TEXT_MIN)),
                ("glyph", J::Float(measure::GLYPH_MIN)),
                ("card", J::Float(measure::CARD_MIN)),
                ("rhythm_pt", J::Float(measure::RHYTHM_PT)),
                ("grey_inked", J::Float(INKED)),
                ("grey_apart", J::Float(APART_MIN)),
                ("hollow_pt2", J::Float(HOLLOW_MIN)),
            ]),
        ),
        ("frames", J::Arr(frames)),
        (
            "not_taken",
            J::Arr(
                not_taken
                    .iter()
                    .map(|(n, why)| {
                        J::obj([("frame", J::s(n.as_str())), ("why", J::s(why.as_str()))])
                    })
                    .collect(),
            ),
        ),
        ("totals", J::s(totals.as_str())),
    ]);
    crate::util::write_out(
        &out.join("report.json"),
        (crate::trail_contrast::render(&json, 0) + "\n").as_bytes(),
    )
    .map_err(|e| format!("report.json: {e}"))?;
    Ok(not_taken.is_empty() && passed == names.len())
}

#[cfg(test)]
#[path = "comprehension_gallery_tests.rs"]
mod tests;
