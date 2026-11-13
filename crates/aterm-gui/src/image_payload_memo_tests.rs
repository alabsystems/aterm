// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;

use aterm_core::grid::extra::{ImageData, ImageFormat, ImageScaling};

use super::{ENTRY_BYTES, PayloadMemo};
use crate::control::control_media::MAX_IMAGE_PAYLOAD_BYTES;
use crate::work_counts::take_images_encoded;

fn image(fill: u8, len: usize) -> Arc<ImageData> {
    Arc::new(ImageData {
        bytes: vec![fill; len],
        format: ImageFormat::Png,
        cols: 1,
        rows: 1,
        z_index: 0,
        band_lift_px: 0,
        cell_offset_px: (0, 0),
        scaling: ImageScaling::Fit,
        source_rect: None,
    })
}

fn b64_of(img: &ImageData) -> String {
    aterm_codec::base64::encode(&img.bytes).expect("encode")
}

fn charge_of(img: &ImageData) -> usize {
    ENTRY_BYTES + b64_of(img).len()
}

#[test]
fn three_images_are_encoded_once_across_frames() {
    let memo = PayloadMemo::with_budget(1 << 20);
    let images = [image(1, 30), image(2, 31), image(3, 32)];
    let refs: Vec<&Arc<ImageData>> = images.iter().collect();
    take_images_encoded();

    let first = memo.payloads_of(&refs);
    assert_eq!(
        take_images_encoded(),
        3,
        "the first frame encodes each image"
    );
    for (payload, img) in first.iter().zip(&images) {
        assert_eq!(payload.format(), "png");
        assert_eq!(payload.b64(), b64_of(img), "the payload is the image's own");
    }
    for _ in 0..3 {
        let again = memo.payloads_of(&refs);
        assert_eq!(take_images_encoded(), 0, "a later frame encodes nothing");
        for (a, b) in again.iter().zip(&first) {
            assert!(
                std::ptr::eq(a.b64().as_ptr(), b.b64().as_ptr()),
                "and shares the first encoding"
            );
        }
    }

    let fresh = PayloadMemo::with_budget(1 << 20);
    fresh.payloads_of(&refs);
    assert_eq!(take_images_encoded(), 3, "the negative control re-encodes");
}

#[test]
fn a_full_memo_keeps_what_the_frame_uses_and_encodes_only_the_rest() {
    let len = 300;
    let images = [image(1, len), image(2, len), image(3, len)];
    let budget = 2 * charge_of(&images[0]);
    let memo = PayloadMemo::with_budget(budget);
    let refs: Vec<&Arc<ImageData>> = images.iter().collect();
    take_images_encoded();

    memo.payloads_of(&refs);
    assert_eq!(take_images_encoded(), 3);
    assert_eq!(memo.entry_count(), 2, "two fit");
    assert!(memo.holds_entry(&images[0]) && memo.holds_entry(&images[1]));
    for _ in 0..3 {
        let payloads = memo.payloads_of(&refs);
        assert_eq!(take_images_encoded(), 1, "only the image that does not fit");
        assert_eq!(payloads[2].b64(), b64_of(&images[2]));
        assert!(memo.charged_bytes() <= budget, "within the budget");
    }

    let big = image(7, 2 * len);
    memo.payloads_of(&[&images[0], &big]);
    assert_eq!(take_images_encoded(), 1);
    assert!(memo.holds_entry(&images[1]), "the older entry stays");
    assert!(!memo.holds_entry(&big), "and the big payload is not kept");

    let next = [image(4, len), image(5, len)];
    let next_refs: Vec<&Arc<ImageData>> = next.iter().collect();
    memo.payloads_of(&next_refs);
    assert_eq!(take_images_encoded(), 2);
    assert!(
        memo.holds_entry(&next[0]) && memo.holds_entry(&next[1]),
        "both kept"
    );
    assert!(!memo.holds_entry(&images[0]) && !memo.holds_entry(&images[1]));
    assert!(memo.charged_bytes() <= budget);
}

#[test]
fn a_batch_never_evicts_an_entry_an_older_batch_kept_for_it() {
    let len = 300;
    let [old, w, x, y] = [image(1, len), image(2, len), image(3, len), image(4, len)];
    let budget = 2 * charge_of(&old);
    let memo = PayloadMemo::with_budget(budget);
    memo.payloads_of(&[&old]);

    let b = memo.begin(&[&x]);
    let a_images = [&w, &x, &y];
    let a = memo.begin(&a_images);
    assert_eq!(a.misses, [0, 1, 2], "A finds none of its images");
    memo.finish(&[&x], b);
    assert!(
        memo.holds_entry(&x),
        "B keeps X, last used by B, which is older than A"
    );
    take_images_encoded();

    let payloads = memo.finish(&a_images, a);
    assert_eq!(take_images_encoded(), 3, "A encodes its misses");
    for (payload, img) in payloads.iter().zip(a_images) {
        assert_eq!(payload.b64(), b64_of(img), "the payload is the image's own");
    }
    assert!(memo.holds_entry(&x), "A does not evict X, which it uses");
    assert!(memo.holds_entry(&w), "W takes the unused entry's room");
    assert!(!memo.holds_entry(&old) && !memo.holds_entry(&y));
    assert!(memo.charged_bytes() <= budget);
}

#[test]
fn a_freed_image_leaves_the_memo_and_its_address_serves_no_stale_payload() {
    let memo = PayloadMemo::with_budget(1 << 20);
    let kept = image(9, 40);
    let freed = image(1, 40);
    memo.payloads_of(&[&kept, &freed]);
    assert_eq!(memo.entry_count(), 2);
    let freed_at = Arc::as_ptr(&freed) as usize;
    drop(freed);

    let other = image(2, 40);
    assert_ne!(
        Arc::as_ptr(&other) as usize,
        freed_at,
        "the entry's Weak holds the freed image's allocation"
    );
    let payloads = memo.payloads_of(&[&other]);
    assert_eq!(
        payloads[0].b64(),
        b64_of(&other),
        "the new image's own bytes"
    );
    assert!(memo.holds_entry(&kept), "a live image keeps its entry");
    assert_eq!(memo.entry_count(), 2, "the freed image's entry is gone");
    assert_eq!(memo.charged_bytes(), charge_of(&kept) + charge_of(&other));
}

#[test]
fn an_oversized_image_is_truncated_and_not_kept() {
    let memo = PayloadMemo::with_budget(1 << 30);
    let big = image(0, MAX_IMAGE_PAYLOAD_BYTES + 1);
    take_images_encoded();
    let payloads = memo.payloads_of(&[&big]);
    assert_eq!(payloads[0].format(), "truncated");
    assert_eq!(payloads[0].b64(), "");
    assert_eq!(take_images_encoded(), 0, "nothing is encoded");
    assert_eq!(memo.entry_count(), 0, "nothing is kept");
}
