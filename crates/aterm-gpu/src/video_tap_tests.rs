// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_render::capture_pixels::display_p3_to_srgb8;

/// Device-free tap for the store and gate laws; `budget_bytes` is taken without the 16 MiB floor.
fn test_tap(budget_bytes: usize, fps_cap: Option<u32>) -> VideoTap {
    VideoTap {
        slots: Vec::new(),
        store: VecDeque::new(),
        store_bytes: 0,
        budget_bytes,
        dropped: 0,
        evicted: 0,
        decimated: 0,
        fps_cap,
        requested_ms: 0,
        next_capture_us: 0,
        epoch: aterm_time::Instant::now(),
        seq: 0,
        half_res: false,
        src_w: 2,
        src_h: 2,
        format: crate::device_layer::TexelFormat::Rgba8Unorm,
        first_capture_encoding: None,
        mixed_capture_encoding: false,
        bpp: 4,
        resized_early_stop: false,
        #[cfg(not(target_arch = "wasm32"))]
        worker: None,
    }
}

/// Finishing a recording hands the conversions still running to its take: the finish
/// returns while one is held, where the UI thread once waited out the whole lane (about 5 s
/// at the end of a debug build's take), and [`VideoTake::settle`] adopts every frame in
/// capture order, a failed conversion counted dropped.
#[test]
#[cfg(any(target_os = "macos", gl_arm))]
fn finishing_a_take_never_waits_on_its_conversions() {
    let (job_tx, job_rx) = mpsc::channel::<ConvertJob>();
    let (result_tx, result_rx) = mpsc::channel::<Option<CapturedFrame>>();
    let (gate_tx, gate_rx) = mpsc::channel::<()>();
    let handle = std::thread::spawn(move || {
        let _jobs = job_rx;
        let _ = gate_rx.recv();
        for seq in [2_u64, 1, 3] {
            let frame = (seq != 3).then(|| CapturedFrame {
                seq,
                t_us: seq,
                w: 1,
                h: 1,
                rgba: vec![0; 4],
            });
            if result_tx.send(frame).is_err() {
                return;
            }
        }
    });
    let mut tap = test_tap(1 << 20, None);
    tap.worker = Some(ConvertWorker {
        job_tx,
        result_rx,
        pending: 3,
        backlog: RING,
        handles: vec![handle],
    });
    #[cfg(target_os = "macos")]
    let finish = VideoTap::metal_finish;
    #[cfg(all(not(target_os = "macos"), gl_arm))]
    let finish = VideoTap::gl_finish;
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = done_tx.send(finish(tap));
    });
    let unsettled = done_rx
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("the finish returned while a conversion was still held");
    assert!(unsettled.take.frames.is_empty(), "nothing converted yet");
    gate_tx.send(()).expect("the worker waits on the gate");
    let take = unsettled.settle();
    let seqs: Vec<u64> = take.frames.iter().map(|frame| frame.seq).collect();
    assert_eq!(seqs, [1, 2], "in capture order");
    assert_eq!(take.dropped, 1, "the failed conversion");
}

fn padded_fixture(rows: &[&[u8]], stride: usize) -> Vec<u8> {
    let mut out = vec![0u8; rows.len() * stride];
    for (row, bytes) in rows.iter().enumerate() {
        out[row * stride..row * stride + bytes.len()].copy_from_slice(bytes);
    }
    out
}

fn half_pixel(channels: [u16; 4]) -> Vec<u8> {
    channels.into_iter().flat_map(u16::to_le_bytes).collect()
}

fn encoding(
    format: crate::device_layer::TexelFormat,
    color_space: CaptureColorSpace,
    sdr_white_scale: f32,
) -> CaptureEncoding {
    CaptureEncoding::new(format, color_space, sdr_white_scale).unwrap()
}

/// The worker's conversion matches inline [`mapped_to_rgba8`], passes stamps through, and exits on sender drop.
#[test]
fn convert_worker_matches_inline_conversion_and_drains_on_drop() {
    let stride = 256;
    let sdr_rows: [&[u8]; 2] = [
        &[1, 2, 3, 255, 9, 8, 7, 128],
        &[0, 0, 0, 0, 255, 255, 255, 255],
    ];
    let sdr_raw = padded_fixture(&sdr_rows, stride);
    let sdr_enc = encoding(
        crate::device_layer::TexelFormat::Rgba8Unorm,
        CaptureColorSpace::Srgb,
        1.0,
    );
    let edr_pixels: Vec<u8> = [
        half_pixel([0x3C00, 0x0000, 0x0000, 0x3C00]),
        half_pixel([0x4400, 0x4400, 0x4400, 0x3C00]),
        half_pixel([0x0000, 0x3800, 0x0000, 0x3C00]),
        half_pixel([0x0000, 0x0000, 0x3C00, 0x3800]),
    ]
    .concat();
    let edr_rows: [&[u8]; 2] = [&edr_pixels[..16], &edr_pixels[16..]];
    let edr_raw = padded_fixture(&edr_rows, stride);
    let edr_enc = encoding(
        crate::device_layer::TexelFormat::Rgba16Float,
        CaptureColorSpace::ExtendedLinearSrgb,
        1.0,
    );
    for (label, raw, format, enc, half_res) in [
        (
            "sdr",
            sdr_raw,
            crate::device_layer::TexelFormat::Rgba8Unorm,
            sdr_enc,
            false,
        ),
        (
            "edr",
            edr_raw,
            crate::device_layer::TexelFormat::Rgba16Float,
            edr_enc,
            true,
        ),
    ] {
        let (inline_rgba, inline_w, inline_h) =
            mapped_to_rgba8(&raw, 2, 2, stride, format, enc, half_res).expect("inline conversion");
        let worker = spawn_convert_worker(2, 2, stride, format, half_res)
            .expect("native test hosts spawn threads");
        worker
            .job_tx
            .send(ConvertJob {
                raw,
                seq: 7,
                t_us: 42,
                capture_encoding: enc,
            })
            .expect("worker alive");
        let frame = worker
            .result_rx
            .recv()
            .expect("worker answers")
            .expect("conversion succeeds");
        assert_eq!(
            (frame.seq, frame.t_us, frame.w, frame.h),
            (7, 42, inline_w, inline_h),
            "{label}: stamps and geometry ride through the worker untouched"
        );
        assert_eq!(
            frame.rgba, inline_rgba,
            "{label}: the off-thread harvest must be byte-identical to inline"
        );
        drop(worker.job_tx);
        for handle in worker.handles {
            handle
                .join()
                .expect("each worker exits when the tap drops its sender");
        }
    }
}

/// Every span [`RawCapture::rgba_row`] appends matches the corresponding slice of [`RawCapture::convert`].
#[test]
fn raw_capture_rows_are_its_conversion() {
    use crate::device_layer::TexelFormat::{Bgra8Unorm, Rgba8Unorm, Rgba16Float};
    let (width, height, stride) = (5u32, 3u32, 256usize);
    let bytes: Vec<u8> = (0..stride * height as usize)
        .map(|i| (i * 89 % 251) as u8)
        .collect();
    for (format, space, scale) in [
        (Bgra8Unorm, CaptureColorSpace::Srgb, 1.0),
        (Rgba8Unorm, CaptureColorSpace::Srgb, 1.0),
        (Rgba8Unorm, CaptureColorSpace::DisplayP3, 1.0),
        (Rgba16Float, CaptureColorSpace::ExtendedLinearSrgb, 2.5),
    ] {
        let label = format!("{format:?} {space:?}");
        let enc = encoding(format, space, scale);
        let raw = RawCapture::new(bytes.clone(), width, height, stride, format, enc, 9)
            .expect("a whole destination");
        let mut rows = Vec::new();
        for y in 0..height as usize {
            for (x, span) in [(0, 5), (1, 3), (4, 1), (2, 0)] {
                rows.clear();
                raw.rgba_row(y, x, span, &mut rows);
                rows.push(0xEE);
                let (whole, _, _) =
                    mapped_to_rgba8(&bytes, width, height, stride, format, enc, false)
                        .expect("converts");
                let start = (y * width as usize + x) * 4;
                let mut expected = whole[start..start + span * 4].to_vec();
                expected.push(0xEE);
                assert_eq!(rows, expected, "{label}: row {y} from {x} for {span}");
            }
            rows.clear();
            raw.rgba_row(y, 3, 3, &mut rows);
            assert!(rows.is_empty(), "{label}: a span past the right edge");
        }
        raw.rgba_row(height as usize, 0, 1, &mut rows);
        assert!(rows.is_empty(), "{label}: a row past the bottom");
        let converted = raw.convert().expect("converts");
        let (whole, w, h) =
            mapped_to_rgba8(&bytes, width, height, stride, format, enc, false).expect("converts");
        assert_eq!(
            (converted.seq, converted.t_us, converted.w, converted.h),
            (1, 9, w, h),
            "{label}"
        );
        assert_eq!(converted.rgba, whole, "{label}");

        let bpp = capture_bpp(format).unwrap() as usize;
        let short = bytes[..(height as usize - 1) * stride + width as usize * bpp - 1].to_vec();
        assert!(
            RawCapture::new(short, width, height, stride, format, enc, 9).is_err(),
            "{label}: a short last row is refused"
        );
    }
}

/// The `should_capture` gate at fps=10 (100 ms spacing); rejections count only `decimated`.
#[test]
fn should_capture_fps10_spacing() {
    let mut tap = test_tap(usize::MAX, Some(10));
    assert!(tap.should_capture(0), "first frame always captured");
    assert!(!tap.should_capture(50_000), "50ms after accept: rejected");
    assert!(tap.should_capture(100_000), "100ms after accept: captured");
    assert!(!tap.should_capture(101_000), "1ms after accept: rejected");
    assert!(!tap.should_capture(199_999), "just under the interval");
    assert!(tap.should_capture(200_000), "100ms after accept: captured");
    assert_eq!(tap.decimated, 3, "each rejection counted as decimated");
    assert_eq!(tap.dropped, 0, "decimation is never a drop");
    assert_eq!(tap.evicted, 0, "decimation is never an eviction");
}

/// No cap = every present captured, nothing decimated.
#[test]
fn should_capture_uncapped_accepts_all() {
    let mut tap = test_tap(usize::MAX, None);
    for t in [0, 1, 2, 1_000] {
        assert!(tap.should_capture(t));
    }
    assert_eq!(tap.decimated, 0);
}

/// Zero fps_cap clamps to 1 fps and near-`u64::MAX` timestamps saturate without panicking.
#[test]
fn should_capture_zero_fps_and_max_timestamp_never_panic() {
    let mut tap = test_tap(usize::MAX, Some(0));
    assert!(tap.should_capture(0));
    assert!(!tap.should_capture(999_999));
    assert!(tap.should_capture(1_000_000));
    assert!(tap.should_capture(u64::MAX - 10));
    assert!(!tap.should_capture(u64::MAX - 1));
    assert!(tap.should_capture(u64::MAX));
}

/// Device-free format contract: both byte-order variants retain source alpha while BGRA swizzles RGB.
#[test]
fn mapped_sdr_formats_preserve_alpha() {
    let rgba_source = [10, 20, 30, 0, 40, 50, 60, 173];
    let (rgba, w, h) = mapped_to_rgba8(
        &rgba_source,
        2,
        1,
        8,
        crate::device_layer::TexelFormat::Rgba8Unorm,
        encoding(
            crate::device_layer::TexelFormat::Rgba8Unorm,
            CaptureColorSpace::Srgb,
            1.0,
        ),
        false,
    )
    .unwrap();
    assert_eq!((w, h), (2, 1));
    assert_eq!(rgba, rgba_source);

    let bgra_source = [30, 20, 10, 7, 60, 50, 40, 241];
    let (rgba, _, _) = mapped_to_rgba8(
        &bgra_source,
        2,
        1,
        8,
        crate::device_layer::TexelFormat::Bgra8Unorm,
        encoding(
            crate::device_layer::TexelFormat::Bgra8Unorm,
            CaptureColorSpace::Srgb,
            1.0,
        ),
        false,
    )
    .unwrap();
    assert_eq!(rgba, [10, 20, 30, 7, 40, 50, 60, 241]);
}

/// f16 RGB converts from extended-linear to sRGB8 while alpha remains linear coverage.
#[test]
fn mapped_f16_preserves_linear_alpha() {
    let mut source = half_pixel([0x3c00, 0x3800, 0x0000, 0x3800]);
    source.extend(half_pixel([0x0000, 0x3c00, 0x3800, 0x3400]));
    let (rgba, w, h) = mapped_to_rgba8(
        &source,
        2,
        1,
        16,
        crate::device_layer::TexelFormat::Rgba16Float,
        encoding(
            crate::device_layer::TexelFormat::Rgba16Float,
            CaptureColorSpace::ExtendedLinearSrgb,
            1.0,
        ),
        false,
    )
    .unwrap();
    assert_eq!((w, h), (2, 1));
    assert_eq!(rgba, [255, 188, 0, 128, 0, 255, 188, 64]);
}

/// The 2×2 downscale averages alpha across every supported format on padded rows.
#[test]
fn half_res_preserves_alpha_for_every_format() {
    let rgba_rows: [&[u8]; 2] = [
        &[40, 50, 60, 0, 40, 50, 60, 64],
        &[40, 50, 60, 128, 40, 50, 60, 255],
    ];
    let rgba_source = padded_fixture(&rgba_rows, 256);
    let (rgba, w, h) = mapped_to_rgba8(
        &rgba_source,
        2,
        2,
        256,
        crate::device_layer::TexelFormat::Rgba8Unorm,
        encoding(
            crate::device_layer::TexelFormat::Rgba8Unorm,
            CaptureColorSpace::Srgb,
            1.0,
        ),
        true,
    )
    .unwrap();
    assert_eq!((w, h), (1, 1));
    assert_eq!(rgba, [40, 50, 60, 112]);

    let bgra_rows: [&[u8]; 2] = [
        &[60, 50, 40, 0, 60, 50, 40, 64],
        &[60, 50, 40, 128, 60, 50, 40, 255],
    ];
    let bgra_source = padded_fixture(&bgra_rows, 256);
    let (bgra, _, _) = mapped_to_rgba8(
        &bgra_source,
        2,
        2,
        256,
        crate::device_layer::TexelFormat::Bgra8Unorm,
        encoding(
            crate::device_layer::TexelFormat::Bgra8Unorm,
            CaptureColorSpace::Srgb,
            1.0,
        ),
        true,
    )
    .unwrap();
    assert_eq!(bgra, [40, 50, 60, 112]);

    let mut f16_row0 = half_pixel([0x0000, 0x0000, 0x0000, 0x0000]);
    f16_row0.extend(half_pixel([0x0000, 0x0000, 0x0000, 0x3800]));
    let mut f16_row1 = half_pixel([0x0000, 0x0000, 0x0000, 0x3c00]);
    f16_row1.extend(half_pixel([0x0000, 0x0000, 0x0000, 0x3400]));
    let f16_source = padded_fixture(&[&f16_row0, &f16_row1], 256);
    let (f16, _, _) = mapped_to_rgba8(
        &f16_source,
        2,
        2,
        256,
        crate::device_layer::TexelFormat::Rgba16Float,
        encoding(
            crate::device_layer::TexelFormat::Rgba16Float,
            CaptureColorSpace::ExtendedLinearSrgb,
            1.0,
        ),
        true,
    )
    .unwrap();
    assert_eq!(f16, [0, 0, 0, 112]);
}

#[test]
fn half_res_filters_linear_premultiplied_rgb() {
    let transparent_blue = [0, 0, 255, 0];
    let opaque_red = [255, 0, 0, 255];
    let rgba_source = padded_fixture(
        &[
            &[opaque_red, transparent_blue].concat(),
            &[opaque_red, transparent_blue].concat(),
        ],
        256,
    );
    let (rgba, _, _) = mapped_to_rgba8(
        &rgba_source,
        2,
        2,
        256,
        crate::device_layer::TexelFormat::Rgba8Unorm,
        encoding(
            crate::device_layer::TexelFormat::Rgba8Unorm,
            CaptureColorSpace::Srgb,
            1.0,
        ),
        true,
    )
    .unwrap();
    assert_eq!(rgba, [255, 0, 0, 128]);
    assert_ne!(
        rgba,
        [127, 0, 127, 128],
        "negative control: transparent blue must not create a purple fringe"
    );

    let black = [0, 0, 0, 255];
    let white = [255, 255, 255, 255];
    let grayscale = padded_fixture(&[&[black, white].concat(), &[black, white].concat()], 256);
    let (rgba, _, _) = mapped_to_rgba8(
        &grayscale,
        2,
        2,
        256,
        crate::device_layer::TexelFormat::Rgba8Unorm,
        encoding(
            crate::device_layer::TexelFormat::Rgba8Unorm,
            CaptureColorSpace::Srgb,
            1.0,
        ),
        true,
    )
    .unwrap();
    assert_eq!(rgba, [188, 188, 188, 255]);
    assert_ne!(
        rgba,
        [127, 127, 127, 255],
        "negative control: gamma-space averaging darkens black/white"
    );

    let low_alpha = padded_fixture(&[&[1, 2, 3, 1, 1, 2, 3, 1], &[1, 2, 3, 1, 1, 2, 3, 0]], 256);
    let (rgba, _, _) = mapped_to_rgba8(
        &low_alpha,
        2,
        2,
        256,
        crate::device_layer::TexelFormat::Rgba8Unorm,
        encoding(
            crate::device_layer::TexelFormat::Rgba8Unorm,
            CaptureColorSpace::Srgb,
            1.0,
        ),
        true,
    )
    .unwrap();
    assert_eq!(rgba[3], 1);
}

#[test]
fn half_res_retains_odd_edges_and_one_pixel_axes() {
    let black = [0, 0, 0, 255];
    let green = [0, 255, 0, 255];
    let rows = [
        &[black, black, black].concat(),
        &[black, black, black].concat(),
        &[black, black, green].concat(),
    ];
    let source = padded_fixture(&[rows[0], rows[1], rows[2]], 256);
    let (rgba, w, h) = mapped_to_rgba8(
        &source,
        3,
        3,
        256,
        crate::device_layer::TexelFormat::Rgba8Unorm,
        encoding(
            crate::device_layer::TexelFormat::Rgba8Unorm,
            CaptureColorSpace::Srgb,
            1.0,
        ),
        true,
    )
    .unwrap();
    assert_eq!((w, h), (2, 2));
    assert_eq!(&rgba[12..16], &green);

    let one = [9, 8, 7, 3];
    let (rgba, w, h) = mapped_to_rgba8(
        &one,
        1,
        1,
        4,
        crate::device_layer::TexelFormat::Rgba8Unorm,
        encoding(
            crate::device_layer::TexelFormat::Rgba8Unorm,
            CaptureColorSpace::Srgb,
            1.0,
        ),
        true,
    )
    .unwrap();
    assert_eq!((w, h), (1, 1));
    assert_eq!(rgba, one);
}

/// The Windows scRGB multiplier is presentation metadata, not scene exposure.
#[test]
fn scrgb_capture_undoes_sdr_white_scale_before_encoding() {
    let source = half_pixel([0x3c00, 0x3800, 0x0000, 0x3800]);
    let decode = |scale| {
        mapped_to_rgba8(
            &source,
            1,
            1,
            8,
            crate::device_layer::TexelFormat::Rgba16Float,
            encoding(
                crate::device_layer::TexelFormat::Rgba16Float,
                CaptureColorSpace::ExtendedLinearSrgb,
                scale,
            ),
            false,
        )
        .unwrap()
        .0
    };
    assert_eq!(decode(2.0), [188, 137, 0, 128]);
    assert_eq!(
        decode(1.0),
        [255, 188, 0, 128],
        "negative control: treating scaled scRGB as unit-linear clips the red channel"
    );
}

/// Over-white channels are compressed together, retaining their ratios instead of independent clamping.
#[test]
fn scrgb_highlight_tone_map_is_bounded_and_chromatic() {
    let source = half_pixel([0x4400, 0x4000, 0x3c00, 0x3400]);
    let (rgba, _, _) = mapped_to_rgba8(
        &source,
        1,
        1,
        8,
        crate::device_layer::TexelFormat::Rgba16Float,
        encoding(
            crate::device_layer::TexelFormat::Rgba16Float,
            CaptureColorSpace::ExtendedLinearSrgb,
            2.0,
        ),
        false,
    )
    .unwrap();
    assert_eq!(rgba, [255, 188, 137, 64]);
    assert_ne!(
        rgba,
        [255, 255, 188, 64],
        "negative control: direct channel clamp destroys highlight chromaticity"
    );
}

/// P3-tagged coordinates must become actual sRGB before an unprofiled PNG consumes them.
#[test]
fn display_p3_capture_converts_primaries_and_srgb_is_byte_exact() {
    let source = [128, 255, 0, 73];
    let p3 = mapped_to_rgba8(
        &source,
        1,
        1,
        4,
        crate::device_layer::TexelFormat::Rgba8Unorm,
        encoding(
            crate::device_layer::TexelFormat::Rgba8Unorm,
            CaptureColorSpace::DisplayP3,
            1.0,
        ),
        false,
    )
    .unwrap()
    .0;
    assert_eq!(p3, [56, 255, 0, 73]);

    let srgb = mapped_to_rgba8(
        &source,
        1,
        1,
        4,
        crate::device_layer::TexelFormat::Rgba8Unorm,
        encoding(
            crate::device_layer::TexelFormat::Rgba8Unorm,
            CaptureColorSpace::Srgb,
            1.0,
        ),
        false,
    )
    .unwrap()
    .0;
    assert_eq!(srgb, source);
    assert_ne!(p3, srgb, "negative control: P3 cannot be passed through");

    assert_eq!(display_p3_to_srgb8([0, 0, 0]), [0, 0, 0]);
    assert_eq!(display_p3_to_srgb8([255, 255, 255]), [255, 255, 255]);
}

#[test]
fn capture_encoding_rejects_format_space_mismatch_and_bad_scale() {
    assert!(
        CaptureEncoding::new(
            crate::device_layer::TexelFormat::Rgba16Float,
            CaptureColorSpace::Srgb,
            1.0,
        )
        .is_err()
    );
    assert!(
        CaptureEncoding::new(
            crate::device_layer::TexelFormat::Bgra8Unorm,
            CaptureColorSpace::ExtendedLinearSrgb,
            1.0,
        )
        .is_err()
    );
    assert!(
        CaptureEncoding::new(
            crate::device_layer::TexelFormat::Rgba16Float,
            CaptureColorSpace::ExtendedLinearSrgb,
            f32::NAN,
        )
        .is_err()
    );
    assert!(
        CaptureEncoding::new(
            crate::device_layer::TexelFormat::Bgra8Unorm,
            CaptureColorSpace::Unknown,
            1.0,
        )
        .is_err()
    );
    assert!(
        presented_snapshot_len(
            u32::MAX,
            1 << 30,
            crate::device_layer::TexelFormat::Rgba16Float,
        )
        .is_err()
    );
}

/// A recording whose surface encoding changes mid-capture keeps the first encoding and marks the recording mixed.
#[test]
fn a_mid_recording_encoding_change_marks_the_recording_mixed() {
    let srgb = encoding(
        crate::device_layer::TexelFormat::Rgba8Unorm,
        CaptureColorSpace::Srgb,
        1.0,
    );
    let p3 = encoding(
        crate::device_layer::TexelFormat::Rgba8Unorm,
        CaptureColorSpace::DisplayP3,
        1.0,
    );
    let mut tap = test_tap(usize::MAX, None);
    tap.note_capture_encoding(srgb);
    assert!(!tap.mixed_capture_encoding);
    tap.note_capture_encoding(p3);
    assert_eq!(tap.first_capture_encoding, Some(srgb));
    assert!(tap.mixed_capture_encoding);
}

#[test]
fn invalid_live_metadata_counts_only_accepted_fps_opportunities() {
    let mut tap = test_tap(usize::MAX, Some(10));
    for now_us in [0, 10_000, 50_000, 100_000] {
        assert!(
            tap.capture_encoding_for_present(now_us, CaptureColorSpace::Unknown, 1.0)
                .is_none()
        );
    }
    assert_eq!(tap.dropped, 2, "t=0 and t=100ms were accepted then lost");
    assert_eq!(
        tap.decimated, 2,
        "t=10ms and t=50ms were client-requested decimation, not losses"
    );
}
