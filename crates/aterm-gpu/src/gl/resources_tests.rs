// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::encoder::GlFrame;
use super::super::test_device as device;
use super::*;
use crate::device_layer::{ClearColor4, FrameLoad};

fn clear(dev: &GlDevice, tex: &GlTexture, [r, g, b, a]: [f64; 4]) {
    let uniforms = dev
        .buffer(GlBufferRole::Uniform, 256)
        .expect("a uniform buffer");
    let mut frame = GlFrame::begin(dev).expect("a frame");
    let load = FrameLoad::Clear(ClearColor4 { r, g, b, a });
    frame
        .begin_pass(tex, false, load, None, (&uniforms, 0))
        .expect("a pass");
    frame.submit().expect("submitted");
}

fn retired(dev: &GlDevice) -> Vec<[GLuint; 2]> {
    dev.shared()
        .retired
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

#[test]
fn a_resized_texture_keeps_its_storage_while_that_holds_the_new_size() {
    let dev = device();
    let rgba = TexelFormat::Rgba8Unorm;
    let mut slot = None;
    assert!(
        dev.texture_2d_resizing(&mut slot, rgba, 100, 80, None)
            .expect("minted")
    );
    let first = slot.as_ref().expect("minted");
    assert_eq!(
        (first.width, first.height, first.stored),
        (100, 80, (100, 80))
    );
    let first = first.name;
    assert!(
        !dev.texture_2d_resizing(&mut slot, rgba, 100, 80, None)
            .expect("kept"),
        "the same size keeps its texels"
    );

    assert!(
        dev.texture_2d_resizing(&mut slot, rgba, 200, 90, None)
            .expect("grown")
    );
    let grown = slot.as_ref().expect("grown");
    assert_ne!(grown.name, first);
    assert_eq!(
        (grown.width, grown.height, grown.stored),
        (200, 90, (256, 128)),
        "a quarter more per side, in 64s"
    );
    let grown = grown.name;
    assert_eq!(
        retired(&dev),
        [[first, 0]],
        "the old one waits for the frame"
    );
    dev.free_retired();
    assert!(retired(&dev).is_empty());

    assert!(
        dev.texture_2d_resizing(&mut slot, rgba, 256, 128, None)
            .expect("refit")
    );
    clear(&dev, slot.as_ref().expect("refit"), [1.0, 0.0, 0.0, 1.0]);
    assert!(
        dev.texture_2d_resizing(&mut slot, rgba, 150, 70, None)
            .expect("refit")
    );
    let inside = slot.as_ref().expect("refit");
    assert_eq!((inside.name, inside.width, inside.height), (grown, 150, 70));
    assert!(inside.roomy());
    clear(&dev, inside, [0.0, 0.0, 1.0, 1.0]);
    assert!(
        dev.texture_2d_resizing(&mut slot, rgba, 256, 128, None)
            .expect("refit")
    );
    let whole = slot.as_ref().expect("refit");
    assert_eq!(whole.name, grown, "nothing minted inside the storage");
    let texels = dev.read_rgba8(whole, 256, 128, 256 * 4).expect("read back");
    for (i, px) in texels.as_chunks::<4>().0.iter().enumerate() {
        let (x, y) = (i % 256, i / 256);
        let want: [u8; 4] = if x < 150 && y < 70 {
            [0, 0, 255, 255]
        } else {
            [255, 0, 0, 255]
        };
        assert_eq!(
            *px, want,
            "texel ({x}, {y}): the clear kept to the texels in use"
        );
    }
    assert!(retired(&dev).is_empty());

    assert!(
        dev.texture_2d_resizing(&mut slot, rgba, 100, 60, None)
            .expect("shrunk")
    );
    let shrunk = slot.as_ref().expect("shrunk");
    assert_ne!(
        shrunk.name, grown,
        "under a quarter of the storage mints again"
    );
    assert_eq!(shrunk.stored, (128, 128));
    assert_eq!(retired(&dev).len(), 1);
    let shrunk = shrunk.name;
    let r8 = TexelFormat::R8Unorm;
    assert!(
        dev.texture_2d_resizing(&mut slot, r8, 100, 60, None)
            .expect("reformatted")
    );
    assert_ne!(
        slot.as_ref().expect("reformatted").name,
        shrunk,
        "another format mints"
    );
    dev.free_retired();
    assert!(retired(&dev).is_empty());
}
