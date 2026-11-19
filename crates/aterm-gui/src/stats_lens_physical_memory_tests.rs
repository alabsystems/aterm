// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_vitals::model::{MemSample, MemoryPartition};

const PHYSICAL_GIB: u64 = 1 << 30;

pub(super) fn physical_memory_sample() -> MemSample {
    MemSample {
        total: 32 * PHYSICAL_GIB,
        app: 12 * PHYSICAL_GIB,
        wired: 4 * PHYSICAL_GIB,
        compressed: Some(2 * PHYSICAL_GIB),
        cached: 8 * PHYSICAL_GIB,
        free: 4 * PHYSICAL_GIB,
        available: 12 * PHYSICAL_GIB,
        exact: true,
        ..MemSample::default()
    }
}

pub(super) fn physical_memory_overflow() -> MemSample {
    MemSample {
        total: u64::MAX,
        app: u64::MAX,
        wired: u64::MAX,
        compressed: Some(u64::MAX),
        cached: u64::MAX,
        free: u64::MAX,
        ..physical_memory_sample()
    }
}

pub(super) fn physical_memory_scene(sample: MemSample) -> Snapshot {
    let mut snap = lens_scene(Scene::Calm);
    let resource = &mut snap.resources[Resource::Memory.index()];
    resource.capacity = (sample.total > 0).then_some(sample.total as f64);
    resource.now = Some(sample.used() as f64);
    resource.heat_now = (sample.total > 0).then(|| sample.used() as f32 / sample.total as f32);
    resource.ring = Arc::from([]);
    snap.mem.as_mut().expect("fixture memory").sample = sample;
    snap
}

fn paint_classes(
    snap: &Snapshot,
    width: f64,
    scale: f32,
) -> (Vec<DrawPrim>, Vec<TextBox>, String, f64) {
    let metrics = FaceMetrics::helvetica_neue();
    let section = classes_section(Ctx {
        two: false,
        cw: width,
        ..two_columns(snap, &metrics)
    });
    let inks = dark_inks();
    let mut pen = Pen {
        p: Painter::new(&inks, scale, 13.0 * scale, false),
        m: &metrics,
        inks,
        ic: false,
        s: f64::from(scale),
        ox: 0.0,
        oy: 0.0,
        draw: true,
        line_tab: 0.0,
        chips: None,
        boxes: Vec::new(),
    };
    let height = section.forms[0].h;
    (section.forms[0].draw)(
        &mut pen,
        Place {
            x: 0.0,
            y: 0.0,
            w: width,
            h: height,
        },
    );
    (pen.p.prims, pen.boxes, section.words.join(" "), height)
}

fn physical_bar(prims: &[DrawPrim], scale: f32) -> Vec<(f32, f32)> {
    prims
        .iter()
        .filter_map(|prim| match prim {
            DrawPrim::Panel { x, y, w, h, .. } if *y == 16.0 * scale && *h == 10.0 * scale => {
                Some((*x, *w))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn physical_memory_six_segments_conserve_capacity_and_advance_past_free() {
    let snap = physical_memory_scene(physical_memory_sample());
    let (prims, _, semantic, _) = paint_classes(&snap, 320.0, 1.0);
    assert_eq!(
        physical_bar(&prims, 1.0),
        [
            (0.0, 120.0),
            (120.0, 40.0),
            (160.0, 20.0),
            (180.0, 80.0),
            (260.0, 40.0),
            (300.0, 20.0),
        ]
    );
    assert!(semantic.contains("free 4.0 GiB"), "{semantic}");
    assert!(semantic.contains("not in VM 2.0 GiB"), "{semantic}");
    let hatch: Vec<_> = prims
        .iter()
        .filter_map(|prim| match prim {
            DrawPrim::Line {
                x1,
                y1,
                x2,
                y2,
                width,
                ..
            } if *y1 < 26.0 && *y2 >= 16.0 => Some((*x1, *y1, *x2, *y2, *width)),
            _ => None,
        })
        .collect();
    assert!(
        !hatch.is_empty(),
        "the residual differs from the free segment"
    );
    for (x1, y1, x2, y2, width) in hatch {
        for (x, y) in [(x1, y1), (x2, y2)] {
            assert!(x - width / 2.0 >= 300.0 && x + width / 2.0 <= 320.0);
            assert!(y - width / 2.0 >= 16.0 && y + width / 2.0 <= 26.0);
        }
    }
}

#[test]
fn physical_memory_available_stored_and_process_charges_cannot_move_a_segment() {
    let mut snap = physical_memory_scene(physical_memory_sample());
    let (before, boxes, semantic, _) = paint_classes(&snap, 320.0, 1.0);
    let sample = &mut snap.mem.as_mut().unwrap().sample;
    sample.available = u64::MAX;
    sample.stored = Some(20 * PHYSICAL_GIB);
    for process in &mut snap.programs {
        process.footprint = Some(u64::MAX);
    }
    for process in &mut snap.top {
        process.footprint = Some(u64::MAX);
    }
    let (after, after_boxes, after_semantic, _) = paint_classes(&snap, 320.0, 1.0);
    assert_eq!(physical_bar(&before, 1.0), physical_bar(&after, 1.0));
    assert_eq!(boxes, after_boxes);
    assert_eq!(semantic, after_semantic);
}

#[test]
fn physical_memory_unproved_readings_keep_numbers_without_a_proportional_bar() {
    let original = physical_memory_sample();
    let mut unknown = original;
    unknown.compressed = None;
    let mut inexact = original;
    inexact.exact = false;
    let mut overfull = original;
    overfull.app = 32 * PHYSICAL_GIB;
    let mut zero = original;
    zero.total = 0;
    let overflow = physical_memory_overflow();
    for sample in [unknown, inexact, overfull, zero, overflow] {
        let projected = sample.physical_classes(Some(Platform::MacOs));
        assert_ne!(projected.partition, MemoryPartition::Exact);
        let snap = physical_memory_scene(sample);
        let (prims, boxes, _, _) = paint_classes(&snap, 136.0, 1.0);
        assert!(physical_bar(&prims, 1.0).is_empty());
        let visible = boxes
            .iter()
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        if sample.compressed.is_none() {
            assert!(visible.contains("compressed not read"), "{visible}");
        }
        if sample == overfull {
            // No individual class is 18 GiB: these digits prove the excess stayed visible.
            assert!(visible.contains("18.0 GiB"), "{visible}");
        }
        assert!(visible.contains("unclassified"), "{visible}");
    }
}

#[test]
fn physical_memory_wrapped_legends_keep_all_classes_inside_their_measured_form() {
    let sample = physical_memory_sample();
    let mut incomplete = sample;
    incomplete.compressed = None;
    let mut overfull = sample;
    overfull.app = 32 * PHYSICAL_GIB;
    for sample in [sample, incomplete, overfull, physical_memory_overflow()] {
        let snap = physical_memory_scene(sample);
        for width in [136.0, 260.0, 500.0] {
            for scale in [1.0, 1.5, 2.0] {
                let (prims, boxes, _, height) = paint_classes(&snap, width, scale);
                let swatches = prims
                    .iter()
                    .filter(|prim| {
                        matches!(prim,
                            DrawPrim::Panel { w, h, .. } if *w == 6.0 * scale && *h == 6.0 * scale
                        )
                    })
                    .count();
                assert_eq!(swatches, 6, "{width}/{scale}");
                for text in &boxes {
                    let [x0, y0, x1, y1] = text.r;
                    assert!(
                        x0 >= -EPS && y0 >= -EPS && x1 <= width + EPS && y1 <= height + EPS,
                        "{}: {:?} outside {width}x{height}",
                        text.text,
                        text.r
                    );
                    assert!(
                        !text.text.contains('…'),
                        "a class value was truncated: {}",
                        text.text
                    );
                }
                for (i, a) in boxes.iter().enumerate() {
                    for b in &boxes[i + 1..] {
                        assert!(!overlap(&a.r, &b.r), "{} meets {}", a.text, b.text);
                    }
                }
                let heard = lay_lens(
                    &shell(526.0, 400.0, scale),
                    &input(&snap, dark_inks()),
                    &pinned(Instrument::Memory),
                    &FaceMetrics::helvetica_neue(),
                    false,
                );
                let semantic = well(&heard, "Where it is");
                let free = format!("free {}", words::mem(sample.free));
                assert!(semantic.contains(&free), "{semantic}");
                assert!(
                    semantic.contains("not in VM") || semantic.contains("unclassified"),
                    "{semantic}"
                );
            }
        }
    }
}
