// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The encoder's working set, measured: an encode holds its input, its output
//! and at most `aterm_png::encoder_scratch_len` bytes — never a filtered or
//! compressed copy of the image. The counting allocator applies to this test
//! binary alone, which holds this one test so nothing else allocates while it
//! measures.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

/// System allocator that tracks live bytes and their high-water mark. It keeps
/// `GlobalAlloc::realloc`'s default (allocate, copy, free), so a buffer's
/// growth counts its old and new blocks together.
struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
        PEAK.fetch_max(live, Ordering::Relaxed);
        // SAFETY: forwarding to the System allocator with the same layout.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: forwarding to the System allocator with the same pointer and
        // layout.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// The payload lengths of a PNG's `IDAT` chunks, in order.
fn idat_lengths(png: &[u8]) -> Vec<usize> {
    let mut rest = &png[8..];
    let mut out = Vec::new();
    while rest.len() >= 12 {
        let len = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
        if &rest[4..8] == b"IDAT" {
            out.push(len);
        }
        rest = &rest[12 + len..];
    }
    out
}

/// RGBA images of about 4 MiB encoded into a sink reserved to
/// `max_encoded_len` allocate at most `encoder_scratch_len` past their input
/// and sink, in one write and a row at a time: a gradient, whose stream is one
/// `IDAT`; noise, whose stream is many `IDAT`s flushed while it is made; and
/// noise in rows wider than the compressor takes per pass. NEGATIVE CONTROL:
/// the allowance is under a quarter of each image and under the noise's
/// compressed stream, so an encoder that held a filtered copy (one byte per
/// pixel byte, plus filter bytes) or the whole compressed stream before
/// writing it could not pass.
#[test]
fn an_encode_holds_no_copy_of_the_image() {
    let mut state = 0x2545_F491u32;
    for (shape, w, h) in [
        ("gradient", 1024u32, 1024u32),
        ("noise", 1024, 1024),
        ("wide noise", 9000, 128),
    ] {
        let stride = w as usize * 4;
        let rgba: Vec<u8> = (0..stride * h as usize)
            .map(|i| match shape {
                "gradient" => ((i % stride) / 4 + (i / stride) * 3 + (i % 4) * 85) as u8,
                _ => {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    (state >> 24) as u8
                }
            })
            .collect();
        let bound = aterm_png::max_encoded_len(w, h, aterm_png::ColorType::Rgba).unwrap();
        let scratch = aterm_png::encoder_scratch_len(w, aterm_png::ColorType::Rgba).unwrap();
        assert!(
            scratch * 4 < rgba.len(),
            "{shape}: the allowance ({scratch}) is too large to tell a copy of the image apart"
        );

        for by_rows in [false, true] {
            let mut png = Vec::with_capacity(bound);
            let base = LIVE.load(Ordering::Relaxed);
            PEAK.store(base, Ordering::Relaxed);
            {
                let mut encoder = aterm_png::Encoder::new(&mut png, w, h);
                encoder.set_color(aterm_png::ColorType::Rgba);
                let mut writer = encoder.write_header().expect("header");
                if by_rows {
                    writer
                        .write_rows(|y, row| {
                            row.extend_from_slice(&rgba[y * stride..(y + 1) * stride]);
                        })
                        .expect("rows");
                } else {
                    writer.write_image_data(&rgba).expect("data");
                }
                writer.finish().expect("finish");
            }
            let extra = PEAK.load(Ordering::Relaxed) - base;
            assert_eq!(png.capacity(), bound, "{shape}: the sink never reallocated");
            assert!(
                extra <= scratch,
                "{shape} by_rows={by_rows}: the encode held {extra} bytes past its input \
                 and sink, over its {scratch}-byte scratch"
            );
            let idats = idat_lengths(&png);
            if shape != "gradient" {
                assert!(
                    idats.len() > 1 && idats.iter().sum::<usize>() > scratch,
                    "{shape}: the stream ({idats:?}) spans several IDATs and outweighs \
                     the allowance"
                );
            }
            let (back, bw, bh) = aterm_png::decode_rgba8(&png, w).expect("decodes");
            assert_eq!((bw, bh), (w as usize, h as usize));
            assert!(
                back == rgba,
                "{shape} by_rows={by_rows}: the pixels round-trip"
            );
        }
    }
}
