// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Full semantic snapshots and overlapping, physically painted scroll coverage.

use super::*;
use crate::system_state_measure::{Layout, parse_layout};
use std::collections::BTreeSet;

fn whole(node: &crate::system_state_measure::Node, layout: &Layout) -> bool {
    let shown = node.undimmed().intersect(layout.viewport);
    node.visible
        && !node.clipped
        && !node.rect.is_empty()
        && (shown.x - node.rect.x).abs() <= 0.1
        && (shown.y - node.rect.y).abs() <= 0.1
        && shown.w + 0.1 >= node.rect.w
        && shown.h + 0.1 >= node.rect.h
}

// A partial scroll-edge node is allowed in an actual viewport, but never
// counts as a painted witness. Every wholly visible monitor node must have
// exact, nontruncated text; every text must belong to a known semantic node.
fn witness(audit: &str, semantic: &BTreeMap<String, String>) -> Option<(BTreeSet<String>, f64)> {
    let layout = parse_layout(audit).ok()?;
    let mut nodes = BTreeMap::new();
    let mut fits = BTreeMap::new();
    for node in layout
        .nodes
        .iter()
        .filter(|node| node.key.starts_with("allocation/monitor/"))
    {
        if semantic.get(&node.key) != Some(&node.label)
            || nodes.insert(node.key.clone(), node).is_some()
        {
            return None;
        }
    }
    for fit in layout
        .texts
        .iter()
        .filter(|fit| fit.key.starts_with("allocation/monitor/"))
    {
        if !nodes.contains_key(&fit.key) || fits.insert(fit.key.clone(), fit).is_some() {
            return None;
        }
    }
    let mut covered = BTreeSet::new();
    for (key, node) in nodes {
        if !whole(node, &layout) {
            continue;
        }
        let fit = fits.get(&key)?;
        if fit.overflow || fit.clip_truncated || fit.painted != node.label {
            return None;
        }
        covered.insert(key);
    }
    let room = layout
        .nodes
        .iter()
        .find(|node| node.key == "allocation/content")?
        .rect
        .h;
    (room.is_finite() && room >= 56.0).then_some((covered, room))
}

struct Frame {
    png: Vec<u8>,
    meta: String,
    audit: String,
    semantic_before: String,
    semantic_after: String,
    covered: BTreeSet<String>,
    keys: BTreeSet<String>,
    room: f64,
}

fn frame(
    inst: &Instance,
    view: u64,
    engine: &mut Engine,
    expected: &Scene<'_>,
    attachment: u64,
) -> Result<Frame, String> {
    let mut wire = inst.connect()?;
    let view = view.to_string();
    let audit_words = ["inspect", "app/v1", "view", view.as_str(), "audit"];
    let text_words = ["inspect", "app/v1", "view", view.as_str(), "text"];
    let until = Instant::now() + HANG;
    let mut previous = None;
    loop {
        engine.live(2)?;
        let _ = relay_reply(&mut wire, &["image", "--bytes"], ReplyShape::Counted)
            .map_err(|e| e.to_string())?;
        let semantic_before = require(
            relay_reply(&mut wire, &text_words, ReplyShape::Counted).map_err(|e| e.to_string())?,
            "semantic snapshot before frame",
        )?;
        let audit = require(
            relay_reply(&mut wire, &audit_words, ReplyShape::Counted).map_err(|e| e.to_string())?,
            "scroll frame audit",
        )?;
        let shape = crate::system_state_gallery::layout_shape(&audit);
        let semantic = monitor_projection(&semantic_before, "text ", "label");
        let before = semantic
            .as_ref()
            .and_then(|semantic| witness(&audit, semantic));
        if !shape.is_empty()
            && previous.as_ref() == Some(&shape)
            && matches_scene(&semantic_before, expected, attachment)
            && let Some((covered, room)) = before
        {
            let shot = require(
                relay_reply(
                    &mut wire,
                    &["image", "--meta", "--bytes"],
                    ReplyShape::Counted,
                )
                .map_err(|e| e.to_string())?,
                "scroll coverage image",
            )?;
            let (png, meta) = image_bytes(&shot)?;
            let after = require(
                relay_reply(&mut wire, &audit_words, ReplyShape::Counted)
                    .map_err(|e| e.to_string())?,
                "scroll frame audit after image",
            )?;
            let semantic_after = require(
                relay_reply(&mut wire, &text_words, ReplyShape::Counted)
                    .map_err(|e| e.to_string())?,
                "semantic snapshot after frame",
            )?;
            let after_rows = monitor_projection(&semantic_after, "text ", "label");
            if crate::system_state_gallery::layout_shape(&after) == shape
                && matches_scene(&semantic_after, expected, attachment)
                && after_rows
                    .as_ref()
                    .and_then(|semantic| witness(&after, semantic))
                    .is_some_and(|(keys, _)| keys == covered)
                && semantic
                    .as_ref()
                    .map(|rows| rows.keys().collect::<BTreeSet<_>>())
                    == after_rows
                        .as_ref()
                        .map(|rows| rows.keys().collect::<BTreeSet<_>>())
            {
                return Ok(Frame {
                    png,
                    meta: meta.to_owned(),
                    audit: after,
                    semantic_before,
                    semantic_after,
                    covered,
                    keys: semantic
                        .expect("witness requires semantic rows")
                        .into_keys()
                        .collect(),
                    room,
                });
            }
        }
        if Instant::now() >= until {
            return Err(format!(
                "scroll frame failed exact semantic/paint validation\n{audit}"
            ));
        }
        previous = Some(shape);
        stop::sleep(Duration::from_millis(100)).map_err(|e| format!("interrupted: {e:?}"))?;
    }
}

fn save(out: &Path, frame: &Frame) -> Result<(), String> {
    for (name, bytes) in [
        ("allocation.png", frame.png.as_slice()),
        ("allocation.layout.txt", frame.audit.as_bytes()),
        ("allocation.image-meta.txt", frame.meta.as_bytes()),
        ("semantic.before.txt", frame.semantic_before.as_bytes()),
        ("semantic.after.txt", frame.semantic_after.as_bytes()),
    ] {
        crate::util::write_fresh(&out.join(name), bytes).map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub(super) fn capture(
    inst: &Instance,
    view: u64,
    out: &Path,
    engine: &mut Engine,
    expected: &Scene<'_>,
    attachment: u64,
    epoch: u64,
) -> Result<(), String> {
    require(
        inst.ctl(&["scroll", "top"]),
        "start Allocation coverage at top",
    )?;
    let focus: BTreeSet<_> = ["availability", "coverage", "work"]
        .into_iter()
        .map(|part| format!("allocation/monitor/epoch/{epoch}/root/0/cache/{part}"))
        .collect();
    let mut required = None;
    let mut all = BTreeSet::new();
    let mut previous = None;
    for step in 0..32 {
        let frame = frame(inst, view, engine, expected, attachment)?;
        if required.as_ref().is_some_and(|keys| keys != &frame.keys) {
            return Err(
                "semantic row inventory changed during fixed-source scroll coverage".into(),
            );
        }
        required.get_or_insert_with(|| frame.keys.clone());
        all.extend(frame.covered.iter().cloned());
        let path = out.join(format!("frame-{step:02}"));
        fs::create_dir(&path).map_err(|e| e.to_string())?;
        save(&path, &frame)?;
        if required.as_ref().is_some_and(|keys| keys.is_subset(&all))
            && focus.is_subset(&frame.covered)
        {
            save(out, &frame)?;
            crate::util::write_fresh(&out.join("paint-coverage.txt"),format!("PASS\nframes={}\nsource_revision={}\nattachment={attachment}\ncovered_keys={all:#?}\nfocused_keys={focus:#?}\n",step+1,expected.snapshot.revision)).map_err(|e|e.to_string())?;
            return Ok(());
        }
        if previous.as_ref() == Some(&frame.covered) {
            let missing: Vec<_> = required
                .as_ref()
                .expect("first frame")
                .difference(&all)
                .collect();
            return Err(format!(
                "Allocation scroll stopped before complete paint coverage: missing={missing:?}, focused={focus:?}"
            ));
        }
        previous = Some(frame.covered);
        // Native Allocation scrolls one line by 28 points. Less than half the
        // actual content room guarantees overlapping views of boundary rows.
        let lines = ((frame.room / 2.0 / 28.0).floor() as i32).max(1);
        require(
            inst.ctl(&["scroll", &(-lines).to_string()]),
            "overlapping Allocation scroll",
        )?;
    }
    Err("Allocation paint coverage exceeded its bounded 32-frame walk".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audit(first_y: f64, second_y: f64, second_text: bool) -> String {
        format!(
            "inspection-source scale=1 viewport=0,0,100,100\npaint-node key=\"allocation/content\" rect=0,0,100,100 clip=0,0,100,100 visible=true clipped=false\npaint-node key=\"allocation/monitor/source\" rect=0,{first_y},100,21 clip=0,0,100,100 visible=true clipped=false label=\"source\"\npaint-text key=\"allocation/monitor/source\" overflow=false clip-truncated=false painted=\"source\"\npaint-node key=\"allocation/monitor/footer\" rect=0,{second_y},100,21 clip=0,0,100,100 visible=true clipped=false label=\"footer\"\n{}",
            if second_text {
                "paint-text key=\"allocation/monitor/footer\" overflow=false clip-truncated=false painted=\"footer\"\n"
            } else {
                ""
            }
        )
    }

    #[test]
    fn partial_scroll_edges_do_not_count_but_overlapping_frames_cover_the_whole_row() {
        let semantic = BTreeMap::from([
            ("allocation/monitor/source".into(), "source".into()),
            ("allocation/monitor/footer".into(), "footer".into()),
        ]);
        let (first, _) = witness(&audit(10.0, 83.0, false), &semantic).unwrap();
        assert_eq!(first, BTreeSet::from(["allocation/monitor/source".into()]));
        let (second, _) = witness(&audit(-10.0, 63.0, true), &semantic).unwrap();
        assert_eq!(second, BTreeSet::from(["allocation/monitor/footer".into()]));
        assert!(
            semantic
                .keys()
                .all(|key| first.union(&second).any(|covered| covered == key))
        );
        assert!(
            witness(&audit(10.0, 63.0, false), &semantic).is_none(),
            "a wholly visible node without paint cannot qualify"
        );
        assert!(
            witness(
                &audit(10.0, 63.0, true)
                    .replace("painted=\"footer\"", "painted=\"different cost\""),
                &semantic
            )
            .is_none()
        );
        assert!(
            witness(
                &audit(10.0, 63.0, true).replace("overflow=false", "overflow=true"),
                &semantic
            )
            .is_none()
        );
        assert!(
            witness(
                &audit(10.0, 63.0, true).replace("clip-truncated=false", "clip-truncated=true"),
                &semantic
            )
            .is_none()
        );
    }
}
