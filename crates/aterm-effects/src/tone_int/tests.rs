// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! tone-int v1 (design §12.2; §12.8 L3 and T1–T5): its derivation, its fidelity to the shipped classifier, its featurization and the session's pins.

use std::collections::HashMap;
use std::env::consts::{ARCH, OS};

use aterm_spec::host::could_not_run;

use super::*;
use crate::int_bag_mlp::{
    Batch, JobBytes, ParamsView, check, forward, forward_fast, out_digest, trace,
};
use crate::tone::{ToneModelError, ToneScratch, builtin, for_each_ngram_bucket};

/// The test-only domain the params golden is digested under.
const GOLDEN_DOMAIN: &[u8] = b"aterm-effects-tone-int-test-v1\0";

/// The target the libm-dependent pins were computed on.
const PINNED_TARGET: &str = "aarch64-apple-darwin";
/// The macOS build (`kern.osversion`) whose libm they were computed with.
const PINNED_OS_BUILD: &str = "26A434";

/// This host's macOS build, `kern.osversion`, or `None` off macOS or where it is unreadable.
fn os_build() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let mut buf = [0u8; 64];
        let mut len: libc::size_t = buf.len();
        // SAFETY: the name is NUL-terminated; `buf` is writable for `len` bytes, the kernel writes at most `len` of them and stores how many in `len`, and no new value is passed (null, 0).
        let rc = unsafe {
            libc::sysctlbyname(
                c"kern.osversion".as_ptr(),
                buf.as_mut_ptr().cast(),
                &raw mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            return None;
        }
        let bytes = &buf[..len.min(buf.len())];
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        std::str::from_utf8(&bytes[..end]).ok().map(str::to_owned)
    }
    #[cfg(not(target_os = "macos"))]
    None
}

/// Hold a figure computed through the system libm to its pin: one that holds passes; one that does not fails on the pinned host and is COULD NOT RUN on any other, whose libm the pin does not describe.
#[track_caller]
fn libm_pin(holds: bool, what: impl std::fmt::Display) {
    if holds {
        return;
    }
    let build = os_build();
    let pinned = cfg!(all(target_os = "macos", target_arch = "aarch64"))
        && build.as_deref() == Some(PINNED_OS_BUILD);
    assert!(
        !pinned,
        "{what}, on the pinned host {PINNED_TARGET} macOS {PINNED_OS_BUILD}"
    );
    could_not_run(format!(
        "{what}: the pin is {PINNED_TARGET} macOS {PINNED_OS_BUILD}'s libm, and this host is {ARCH}-{OS} build {}",
        build.as_deref().unwrap_or("unknown")
    ));
}

fn view() -> ParamsView<'static> {
    check(v1().expect("tone-int v1 derives")).expect("and checks")
}

fn run(p: &ParamsView<'_>, b: &Batch) -> Vec<u32> {
    let mut out = vec![0u32; b.out_words()];
    forward(p, b, &mut out).expect("forward");
    out
}

fn class_of(out: &[u32], row: usize) -> u8 {
    let stride = DIMS.out_stride() as usize;
    out[row * stride + stride - 1] as u8
}

fn shipped_codes() -> Vec<u8> {
    let m = builtin().expect("shipped model");
    let mut s = ToneScratch::default();
    texts()
        .iter()
        .map(|t| class_code(m.classify_opt(t, &mut s)))
        .collect()
}

/// The asset with byte `at` set to `value` and its checksum recomputed.
fn reseal(at: usize, value: u8) -> Vec<u8> {
    let mut bytes = tone::ASSET.to_vec();
    bytes[at] = value;
    let end = bytes.len() - 4;
    let mut h: u32 = 0x811c_9dc5;
    for &b in &bytes[..end] {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    bytes[end..].copy_from_slice(&h.to_le_bytes());
    bytes
}

/// T1: tone-int v1's id and design §12.2's table of the shipped asset's figures.
#[test]
fn the_derivation_figures_and_id_are_pinned() {
    let raw = tone::raw(tone::ASSET).expect("the shipped asset");
    let scales = [raw.emb_scale(), raw.w1_scale(), raw.w2_scale()];
    assert_eq!(
        scales.map(f32::to_bits),
        [0x3cc3_01d4, 0x3c4b_c5ad, 0x3cab_d47c]
    );
    assert_eq!(
        scales.map(|s| format!("{s:.7}")),
        ["0.0238046", "0.0124373", "0.0209753"]
    );
    let range = |v: &[u8]| {
        let all = v.iter().map(|&b| b as i8);
        (all.clone().min(), all.max())
    };
    assert_eq!(range(raw.emb()), (Some(-127), Some(105)));
    assert_eq!(range(raw.w1()), (Some(-127), Some(115)));
    assert_eq!(range(raw.w2()), (Some(-93), Some(127)));
    let p = view();
    assert_eq!(p.dims(), DIMS);
    assert_eq!((p.min_ids(), p.g(), p.t()), (6, 20, 1_024));
    assert_eq!(
        (p.mult(), p.qmax(), p.m(), p.max_abs_b1()),
        (79_474, 27_021, 2_047, 98)
    );
    assert_eq!(p.m() - p.max_abs_b1(), 1_949, "the lossless margin, ≥ T");
    assert_eq!(p.b2(), [-1_402, -10_745, 12_343, -3_793, 3_599]);
    assert_eq!(p.emb(), raw.emb(), "the i8 tensors are verbatim");
    assert_eq!(p.w1(), raw.w1());
    assert_eq!(p.w2(), raw.w2());
    assert_eq!(id(&p).hex(), V1_ID);
}

/// F + G sits on the lossless limit `F + G ≤ 28` that T = 4·2^F allows, and tone-int's constants are the design's.
#[test]
fn the_fixed_point_constants_are_the_designs() {
    assert_eq!((F, G, T), (8, 20, 1_024));
    assert_eq!(F + G, 28);
    assert_eq!(MIN_IDS as usize, tone::MIN_NGRAMS);
    assert_eq!(
        DIMS,
        Dims {
            buckets: 2_048,
            embed: 32,
            hidden: 64,
            classes: 5,
        }
    );
}

/// derive is a pure function of the asset, v1 is derive of the shipped asset, and derive's source calls no libm function.
#[test]
fn derive_is_pure_and_names_no_libm() {
    let a = derive(tone::ASSET).expect("derives");
    assert_eq!(a, derive(tone::ASSET).expect("derives"));
    assert_eq!(v1().expect("v1"), &a[..]);
    let source = include_str!("../tone_int.rs");
    for call in [
        ".tanh(",
        ".exp(",
        ".ln(",
        ".log",
        ".powf(",
        ".powi(",
        ".sqrt(",
        ".round(",
        ".trunc(",
        "f64::tanh",
        "f32::tanh",
        "libm",
    ] {
        for (no, line) in source.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            assert!(!code.contains(call), "line {}: {call}", no + 1);
        }
    }
}

/// No table entry lies within 3.9·10⁻⁴ of a rounding boundary, the rounding takes halves away from zero, and Lambert's fraction is within 8·2⁻⁵² of libm's tanh on the table's grid (2⁻⁵² on the pinned host), far inside the 3.1·10⁻⁶ the table's exactness needs.
#[test]
fn the_table_is_exact_and_its_rounding_is_half_away() {
    let (mut nearest, mut worst) = (f64::MAX, 0f64);
    for i in 0..=2 * T {
        let x = f64::from(i - T) / 256.0;
        worst = worst.max((tanh(x) - x.tanh()).abs());
        let y = 127.0 * tanh(x);
        nearest = nearest.min(((y - y.trunc()).abs() - 0.5).abs());
    }
    assert_eq!(format!("{nearest:.2e}"), "3.91e-4");
    let p = view();
    let lut: Vec<i8> = p.lut().iter().map(|&b| b as i8).collect();
    assert_eq!(lut.len(), 2 * T as usize + 1);
    assert_eq!(
        (lut[0], lut[T as usize], lut[2 * T as usize]),
        (-127, 0, 127)
    );
    for i in 0..=T as usize {
        assert_eq!(lut[T as usize + i], -lut[T as usize - i], "odd");
    }
    assert!(lut.windows(2).all(|w| w[0] <= w[1]), "monotone");
    assert_eq!(tanh(0.0), 0.0);
    for (x, want) in [
        (0.5, 1),
        (-0.5, -1),
        (1.5, 2),
        (2.5, 3),
        (-2.5, -3),
        (0.499_999_999_999_999_94, 0),
        (-0.499_999_999_999_999_94, 0),
        (4_503_599_627_370_495.0, 4_503_599_627_370_495),
    ] {
        assert_eq!(round_half_away(x), Some(want), "{x}");
    }
    for x in [
        f64::NAN,
        f64::INFINITY,
        4_503_599_627_370_496.0,
        -4_503_599_627_370_496.0,
    ] {
        assert_eq!(round_half_away(x), None, "{x}");
    }
    libm_pin(
        worst <= 8.0 * f64::EPSILON,
        format_args!("Lambert's fraction is {worst:e} from libm's tanh"),
    );
}

/// derive refuses an asset the shipped checks refuse, and −128 in each i8 tensor by name.
#[test]
fn derive_refuses_a_bad_asset_and_minus_128() {
    assert_eq!(
        derive(&tone::ASSET[..100]),
        Err(DeriveError::Asset(ToneModelError::BadLength))
    );
    let mut flipped = tone::ASSET.to_vec();
    flipped[1000] ^= 1;
    assert_eq!(
        derive(&flipped),
        Err(DeriveError::Asset(ToneModelError::BadChecksum))
    );
    let emb = 36;
    let w1 = emb + tone::BUCKETS * tone::EMBED;
    let w2 = w1 + tone::EMBED * tone::HIDDEN;
    for (tensor, start, at) in [
        (Tensor::Emb, emb, emb + 10),
        (Tensor::W1, w1, w1 + 3),
        (Tensor::W2, w2, w2),
    ] {
        let bytes = reseal(at, 0x80);
        assert!(
            tone::ToneModel::from_bytes(&bytes).is_ok(),
            "the shipped checks pass"
        );
        assert_eq!(
            derive(&bytes),
            Err(DeriveError::Int8Min {
                tensor,
                index: (at - start) as u32,
            })
        );
    }
}

/// T2: tone-int v1 clears the shipped held-out floors exactly as the shipped test defines them, with abstention scored as Technical, and agrees with the shipped classifier on every line under the pinned host's libm.
#[test]
fn tone_int_clears_the_shipped_floors_and_agrees_with_the_shipped_classifier() {
    let p = view();
    let lines = workload();
    let b = pack(&p, &texts()).expect("the workload packs");
    let out = run(&p, &b);
    let mut total = (0usize, 0usize);
    let mut by_lang: HashMap<&str, (usize, usize)> = HashMap::new();
    for (i, line) in lines.iter().enumerate().filter(|(_, l)| l.held_out) {
        let got = Tone::from_index(usize::from(class_of(&out, i)));
        let e = by_lang.entry(line.lang).or_default();
        e.1 += 1;
        total.1 += 1;
        if got == line.tone {
            e.0 += 1;
            total.0 += 1;
        }
    }
    assert!(
        total.1 >= 60,
        "holdout set went missing ({} lines)",
        total.1
    );
    let overall = total.0 as f32 / total.1 as f32;
    assert!(
        overall >= 0.70,
        "held-out floor broken: {}/{} = {overall:.2} < 0.70",
        total.0,
        total.1
    );
    for lang in ["zh", "ja", "ko", "en", "es"] {
        let &(ok, n) = by_lang.get(lang).unwrap_or(&(0, 0));
        assert!(n >= 5, "holdout must keep covering {lang} (has {n})");
        let acc = ok as f32 / n as f32;
        assert!(
            acc >= 0.60,
            "{lang} floor broken: {ok}/{n} = {acc:.2} < 0.60"
        );
    }
    assert_eq!(total, (61, 71), "held-out lines right");
    let m = builtin().expect("shipped model");
    let mut s = ToneScratch::default();
    let shipped_right = lines
        .iter()
        .filter(|l| l.held_out && m.classify(l.text, &mut s) == l.tone)
        .count();
    libm_pin(
        shipped_right == 61,
        format_args!(
            "the shipped classifier has {shipped_right} of 71 held-out lines right, not 61"
        ),
    );
    let shipped = shipped_codes();
    let agree = |held_out: bool| {
        (0..lines.len())
            .filter(|&i| lines[i].held_out == held_out && class_of(&out, i) == shipped[i])
            .count()
    };
    let agreement = (agree(false), agree(true));
    libm_pin(
        agreement == (300, 71),
        format_args!("argmax agreement {agreement:?}, not (300, 71)"),
    );
}

/// T2: against the f32 path, the largest pre-activation error is 1.54 steps of 2^−F, the smallest top-2 integer margin 676, no row reaches either clamp (largest |p| 670 < T), and the largest logit error 0.181 under the pinned host's libm.
#[test]
fn fidelity_to_the_f32_path_is_pinned() {
    let p = view();
    let m = builtin().expect("shipped model");
    let s2 = f64::from(tone::raw(tone::ASSET).expect("asset").w2_scale());
    let lines = workload();
    let b = pack(&p, &texts()).expect("the workload packs");
    let out = run(&p, &b);
    let stride = DIMS.out_stride() as usize;
    let (mut pre_err, mut logit_err, mut margin, mut max_p) = (0f64, 0f64, i64::MAX, 0i32);
    for (i, line) in lines.iter().enumerate() {
        let t = trace(&p, &b, i as u32)
            .expect("traced")
            .expect("no workload line abstains");
        assert_eq!((t.clamped, t.saturated), (0, 0), "line {i}");
        let (pre, logits) = m.trace(line.text).expect("nor does the f32 path");
        for (&pk, &f) in t.pre.iter().zip(&pre) {
            pre_err = pre_err.max((f64::from(pk) - f64::from(f) * f64::from(1u32 << F)).abs());
            max_p = max_p.max(pk.abs());
        }
        let mut l: Vec<i64> = out[i * stride..i * stride + stride - 1]
            .iter()
            .map(|&w| i64::from(w as i32))
            .collect();
        for (&lc, &f) in l.iter().zip(&logits) {
            logit_err = logit_err.max((lc as f64 * s2 / 127.0 - f64::from(f)).abs());
        }
        l.sort_unstable();
        margin = margin.min(l[l.len() - 1] - l[l.len() - 2]);
    }
    assert_eq!(pre_err, 1.542_194_366_455_078_1, "no libm reaches it");
    assert_eq!(margin, 676);
    assert_eq!(max_p, 670);
    libm_pin(
        (logit_err - 0.181_410_651_448_672_4).abs() < 1e-9,
        format_args!("the largest logit error is {logit_err:?}, not 0.1814106514486724"),
    );
}

/// T3: forward_fast equals forward word for word on the workload, B1 and abstaining lines.
#[test]
fn forward_fast_equals_forward_on_the_workload() {
    let p = view();
    let mut all = texts();
    all.extend(["", "a", "ok", "    "]);
    for batch in [&all[..], &[B1_TEXT][..]] {
        let b = pack(&p, batch).expect("packs");
        let mut fast = vec![0u32; b.out_words()];
        forward_fast(&p, &b, &mut fast).expect("forward_fast");
        assert_eq!(fast, run(&p, &b));
    }
    let b = pack(&p, &all[WORKLOAD_LINES..]).expect("packs");
    let out = run(&p, &b);
    for row in 0..4 {
        assert_eq!(class_of(&out, row), 5, "abstains");
    }
}

/// T4: each packed row is the shipped `for_each_ngram_bucket` buckets of its line.
#[test]
fn packed_ids_are_the_shipped_buckets() {
    let p = view();
    let texts = texts();
    let b = pack(&p, &texts).expect("the workload packs");
    assert_eq!(b.rows() as usize, WORKLOAD_LINES);
    for (i, text) in texts.iter().enumerate() {
        let mut want = Vec::new();
        let n = for_each_ngram_bucket(text, |x| want.push(x as u16));
        assert_eq!(b.row(i as u32), Some(&want[..]), "line {i}");
        assert_eq!(n, want.len());
    }
    let longest = |range: core::ops::Range<usize>| {
        range.map(|i| b.row(i as u32).map_or(0, <[u16]>::len)).max()
    };
    assert_eq!(longest(0..CORPUS_LINES), Some(147));
    assert_eq!(longest(CORPUS_LINES..WORKLOAD_LINES), Some(120));
}

/// T5: the session's pins: the workload's digest, forward's B371 digest, B16384's ids and job bytes, and `F_ARGMAX_371`, the pinned host's libm's.
#[test]
fn the_session_pins_reproduce() {
    let lines = workload();
    assert_eq!(lines.len(), WORKLOAD_LINES);
    assert_eq!(lines.iter().filter(|l| !l.held_out).count(), CORPUS_LINES);
    assert!(lines[..CORPUS_LINES].iter().all(|l| !l.held_out));
    assert_eq!(lines[0].text, B1_TEXT);
    assert_eq!(workload_digest().hex(), WORKLOAD_DIGEST);
    let parts: Vec<&[u8]> = lines.iter().map(|l| l.text.as_bytes()).collect();
    assert_eq!(workload_digest(), Digest::of(WORKLOAD_DOMAIN, &parts));
    let p = view();
    let texts = texts();
    let b = pack(&p, &texts).expect("B371 packs");
    assert_eq!(b.total_ids(), 25_368);
    assert_eq!(out_digest(&run(&p, &b)).hex(), B371_OUT_DIGEST);
    let cyclic: Vec<&str> = (0..16_384).map(|i| texts[i % WORKLOAD_LINES]).collect();
    let big = pack(&p, &cyclic).expect("B16384 packs");
    assert_eq!(big.total_ids(), 1_119_606);
    assert_eq!(
        big.bytes(),
        JobBytes {
            job: 20,
            offsets: 65_540,
            ids: 2_239_212,
            out: 393_216,
        }
    );
    assert_eq!(big.bytes().total(), 2_697_988);
    let codes = shipped_codes();
    let moved = codes
        .iter()
        .zip(F_ARGMAX_371)
        .filter(|(a, b)| **a != *b)
        .count();
    libm_pin(
        codes == F_ARGMAX_371,
        format_args!("the shipped argmax differs from F_ARGMAX_371 on {moved} lines"),
    );
}

/// L3: a fixed three-row batch's offsets, ids and job header bytes, its out words, and tone-int v1's params bytes match their goldens.
#[test]
fn a_three_row_batch_and_the_params_bytes_match_their_goldens() {
    let p = view();
    let b = Batch::pack(
        &p,
        vec![0, 6, 6, 9],
        vec![0, 1, 2047, 1024, 7, 513, 2046, 3, 1000],
    )
    .expect("valid");
    let le = |w: &[u32]| w.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    assert_eq!(
        le(b.offsets()),
        [0, 0, 0, 0, 6, 0, 0, 0, 6, 0, 0, 0, 9, 0, 0, 0]
    );
    let mut ids: Vec<u8> = b.ids().iter().flat_map(|x| x.to_le_bytes()).collect();
    ids.resize(b.bytes().ids as usize, 0);
    assert_eq!(
        ids,
        [
            0, 0, 1, 0, 0xff, 0x07, 0, 0x04, 7, 0, 0x01, 0x02, 0xfe, 0x07, 3, 0, 0xe8, 0x03, 0, 0
        ]
    );
    assert_eq!(
        le(&b.header()),
        [
            b'I', b'B', b'M', b'J', 1, 0, 0, 0, 3, 0, 0, 0, 9, 0, 0, 0, 6, 0, 0, 0
        ]
    );
    let out: Vec<i32> = run(&p, &b).into_iter().map(|w| w as i32).collect();
    assert_eq!(
        out,
        [
            -72_572, 66_355, 49_805, -52_738, 11_091, 1, 0, 0, 0, 0, 0, 5, 0, 0, 0, 0, 0, 5
        ]
    );
    assert_eq!(p.bytes().len(), 70_340);
    assert_eq!(
        Digest::of(GOLDEN_DOMAIN, &[p.bytes()]).hex(),
        "1c3782462ee0fbfb207130c63aa7ea80a9592b3f574c722ab0979faf1def6ceb"
    );
}

/// tone-int's id is the documented descriptor digest, and a flipped params byte moves it.
#[test]
fn the_id_covers_its_descriptor() {
    let p = view();
    let d = p.dims();
    let want = Digest::of(
        b"aterm-effects-tone-int-v1\0",
        &[
            &1u32.to_le_bytes(),
            b"int32-exact/v1",
            &d.buckets.to_le_bytes(),
            &d.embed.to_le_bytes(),
            &d.hidden.to_le_bytes(),
            &d.classes.to_le_bytes(),
            p.bytes(),
        ],
    );
    assert_eq!(id(&p), want);
    let mut bytes = p.bytes().to_vec();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    assert_ne!(id(&check(&bytes).expect("still valid")), want);
}

/// pack_into reuses the vectors a batch gives back, and packs what pack packs.
#[test]
fn pack_into_reuses_the_batchs_vectors() {
    let p = view();
    let texts = texts();
    let b = pack(&p, &texts).expect("packs");
    let (offsets, ids) = b.into_parts();
    let (o_ptr, i_ptr) = (offsets.as_ptr(), ids.as_ptr());
    let small = pack_into(&p, &texts[..3], offsets, ids).expect("packs");
    assert_eq!(small, pack(&p, &texts[..3]).expect("packs"));
    assert_eq!(
        (small.offsets().as_ptr(), small.ids().as_ptr()),
        (o_ptr, i_ptr)
    );
    assert_eq!(pack(&p, &[]), Err(crate::int_bag_mlp::BatchError::Empty));
}
