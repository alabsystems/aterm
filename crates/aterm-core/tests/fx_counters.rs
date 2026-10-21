// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fx engine's cost counters (design §7 "Cost"): a content seal is one
//! `fx_layer_builds`, a deep payload copy one `fx_payload_copies`, and handing
//! the same `Arc` on (a settled layer) moves neither. Its own binary: the
//! counters are process-wide, so the exact deltas need a process no other
//! test builds content in.

use std::sync::Arc;

use aterm_core::render::{
    FxBlend, FxContent, FxInk, FxItem, FxLayer, FxRect, FxSlot, FxSpace, fx_layer_builds,
    fx_payload_copies,
};

fn rect_content() -> FxContent {
    FxContent::new(
        vec![FxItem::Rect(FxRect {
            x: 0,
            y: 0,
            w: 4,
            h: 2,
            ink: 0,
        })],
        vec![
            FxInk {
                color: 0x0010_2030,
                alpha: 255,
            };
            2
        ],
    )
}

#[test]
fn seals_and_deep_copies_are_counted_and_a_settled_layer_moves_neither() {
    let (b0, c0) = (fx_layer_builds(), fx_payload_copies());
    let mut content = Arc::new(rect_content());
    assert_eq!(
        (fx_layer_builds() - b0, fx_payload_copies() - c0),
        (1, 0),
        "a new content is one build and no copy"
    );

    // A settled layer: the host hands the same Arc on, frame after frame.
    let (b1, c1) = (fx_layer_builds(), fx_payload_copies());
    let layer = FxLayer::new(
        1,
        Arc::clone(&content),
        FxSlot::Under,
        FxSpace::Window,
        FxBlend::Exact,
    );
    let frames: Vec<FxLayer> = (0..64).map(|_| layer.clone()).collect();
    assert!(frames.iter().all(|l| *l == layer));
    assert_eq!(
        (fx_layer_builds() - b1, fx_payload_copies() - c1),
        (0, 0),
        "a settled layer seals nothing and copies nothing"
    );
    drop(frames);
    drop(layer);

    // The emitter's ring: the content is unique again, so `make_mut` edits
    // in place: one build, no copy.
    let (b2, c2) = (fx_layer_builds(), fx_payload_copies());
    Arc::make_mut(&mut content).edit(|items, _| {
        if let Some(FxItem::Rect(r)) = items.first_mut() {
            r.w = 5;
        }
    });
    assert_eq!(
        (fx_layer_builds() - b2, fx_payload_copies() - c2),
        (1, 0),
        "an edit of a unique payload is one build and no copy"
    );

    // A payload a frame still holds: `make_mut` copies it first.
    let held = Arc::clone(&content);
    let (b3, c3) = (fx_layer_builds(), fx_payload_copies());
    Arc::make_mut(&mut content).edit(|items, _| {
        if let Some(FxItem::Rect(r)) = items.first_mut() {
            r.w = 6;
        }
    });
    assert_eq!(
        (fx_layer_builds() - b3, fx_payload_copies() - c3),
        (1, 1),
        "an edit of a shared payload is one copy and one build"
    );
    assert!(!Arc::ptr_eq(&held, &content));
}
