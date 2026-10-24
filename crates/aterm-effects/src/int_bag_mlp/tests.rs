// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `int32-exact/v1`'s layout, params checker, packer and CPU reference (design §12.2–12.3; §12.8 L4 and T3).

use std::collections::BTreeSet;

use super::*;

/// An owned [`Spec`].
#[derive(Clone)]
struct Owned {
    dims: Dims,
    min_ids: u32,
    mult: u32,
    g: u32,
    t: i32,
    emb: Vec<i8>,
    w1: Vec<i8>,
    w2: Vec<i8>,
    lut: Vec<i8>,
    b1: Vec<i32>,
    b2: Vec<i32>,
}

impl Owned {
    fn spec(&self) -> Spec<'_> {
        Spec {
            dims: self.dims,
            min_ids: self.min_ids,
            mult: self.mult,
            g: self.g,
            t: self.t,
            emb: &self.emb,
            w1: &self.w1,
            w2: &self.w2,
            lut: &self.lut,
            b1: &self.b1,
            b2: &self.b2,
        }
    }

    fn bytes(&self) -> Vec<u8> {
        build(&self.spec())
    }
}

/// The hand-computed spec: B 4, E 2, H 2, C 2, MULT 2^G (so step 5 keeps q), T 4.
fn small() -> Owned {
    Owned {
        dims: Dims {
            buckets: 4,
            embed: 2,
            hidden: 2,
            classes: 2,
        },
        min_ids: 2,
        mult: 1 << 20,
        g: 20,
        t: 4,
        emb: vec![3, -5, -7, 2, 127, 127, -127, -127],
        w1: vec![2, -1, 1, 3],
        w2: vec![1, 2, 3, -1],
        lut: vec![-100, -50, -20, -10, 0, 10, 20, 50, 100],
        b1: vec![1, -1],
        b2: vec![0, 5],
    }
}

/// `small` with an identity activation table of T = 16.
fn identity(mult: u32) -> Owned {
    Owned {
        mult,
        t: 16,
        lut: (-16..=16).collect(),
        ..small()
    }
}

fn csr(rows: &[&[u16]]) -> (Vec<u32>, Vec<u16>) {
    let mut offsets = vec![0u32];
    let mut ids = Vec::new();
    for row in rows {
        ids.extend_from_slice(row);
        offsets.push(ids.len() as u32);
    }
    (offsets, ids)
}

fn run_both(p: &ParamsView<'_>, b: &Batch) -> Vec<u32> {
    let mut checked = vec![0u32; b.out_words()];
    forward(p, b, &mut checked).expect("forward");
    let mut fast = vec![0u32; b.out_words()];
    forward_fast(p, b, &mut fast).expect("forward_fast");
    assert_eq!(fast, checked, "forward_fast equals forward word for word");
    checked
}

fn words(logits: &[i32], class: u32) -> Vec<u32> {
    logits.iter().map(|&l| l as u32).chain([class]).collect()
}

fn refused(bytes: &[u8]) -> ParamsError {
    check(bytes).expect_err("the checker refuses it")
}

fn patched(o: &Owned, at: usize, value: u32) -> Vec<u8> {
    let mut bytes = o.bytes();
    bytes[at * 4..at * 4 + 4].copy_from_slice(&value.to_le_bytes());
    bytes
}

fn packed(o: &Owned, offsets: Vec<u32>, ids: Vec<u16>) -> Result<Batch, BatchError> {
    let bytes = o.bytes();
    Batch::pack(&check(&bytes).expect("a valid spec"), offsets, ids)
}

/// Both headers' tables are the pinned ones, names unique across both and indices dense in word order.
#[test]
fn layout_tables_have_unique_names_and_dense_indices() {
    assert_eq!(
        JOB_FIELDS,
        [
            ("JOB_MAGIC", 0),
            ("JOB_LAYOUT_VERSION", 1),
            ("JOB_ROWS", 2),
            ("JOB_TOTAL_IDS", 3),
            ("JOB_OUT_STRIDE", 4),
        ]
    );
    assert_eq!(
        PARAMS_FIELDS,
        [
            ("PARAMS_MAGIC", 0),
            ("PARAMS_LAYOUT_VERSION", 1),
            ("PARAMS_B", 2),
            ("PARAMS_E", 3),
            ("PARAMS_H", 4),
            ("PARAMS_C", 5),
            ("PARAMS_MIN_IDS", 6),
            ("PARAMS_MULT", 7),
            ("PARAMS_QMAX", 8),
            ("PARAMS_G", 9),
            ("PARAMS_T", 10),
            ("PARAMS_EMB_OFF", 11),
            ("PARAMS_EMB_LEN", 12),
            ("PARAMS_W1_OFF", 13),
            ("PARAMS_W1_LEN", 14),
            ("PARAMS_W2_OFF", 15),
            ("PARAMS_W2_LEN", 16),
            ("PARAMS_LUT_OFF", 17),
            ("PARAMS_LUT_LEN", 18),
            ("PARAMS_B1_OFF", 19),
            ("PARAMS_B1_LEN", 20),
            ("PARAMS_B2_OFF", 21),
            ("PARAMS_B2_LEN", 22),
        ]
    );
    for table in [LAYOUT.job, LAYOUT.params] {
        for (at, &(_, index)) in table.iter().enumerate() {
            assert_eq!(index, at, "dense, in word order");
        }
    }
    let names: BTreeSet<&str> = LAYOUT.fields().map(|f| f.0).collect();
    assert_eq!(names.len(), JOB_WORDS + PARAMS_WORDS, "unique across both");
    assert_eq!((JOB_WORDS, PARAMS_WORDS), (5, 23));
    for (i, t) in Tensor::ALL.into_iter().enumerate() {
        assert_eq!(t as usize, i, "spans index by tensor");
        let (off, len) = t.fields();
        assert_eq!((off, len), (11 + 2 * i, 12 + 2 * i));
    }
    assert_eq!(JOB_MAGIC_VALUE.to_le_bytes(), *b"IBMJ");
    assert_eq!(PARAMS_MAGIC_VALUE.to_le_bytes(), *b"IBMP");
}

/// The bounds are design §12.2's and §12.5's, and each guarantee holds at them.
#[test]
fn the_bounds_are_the_designs() {
    assert_eq!((B_MAX, E_MAX, H_MAX, C_MAX), (65_536, 32, 128, 16));
    assert_eq!((MAX_ROW_IDS, ROWS_MAX), (4_096, 1 << 20));
    assert_eq!(
        (JOB_BYTES_MAX, PARAMS_BYTES_MAX, DOMAIN_BYTES_MAX),
        (64 << 20, 16 << 20, 256 << 20)
    );
    let row_cap = 127 * 127 * i64::from(E_MAX) * i64::from(MAX_ROW_IDS);
    assert_eq!(row_cap, 2_114_060_288);
    assert_eq!(i64::from(i32::MAX) - row_cap, 33_423_359, "below 2³¹");
    let step7 = 127 * 127 * i64::from(H_MAX) + i64::from(B2_LIMIT);
    assert_eq!(i64::from(i32::MAX) - step7, 1_608_548_223, "below 2³¹");
    assert_eq!(
        (qmax(79_474), qmax(1), qmax(0)),
        (27_021, i32::MAX as u32, 0)
    );
}

/// A built blob checks, and its view reads back the spec at 16-byte-aligned offsets in blob order.
#[test]
fn build_then_check_reads_back_the_spec() {
    let o = small();
    let bytes = o.bytes();
    let p = check(&bytes).expect("valid");
    assert_eq!(p.dims(), o.dims);
    assert_eq!(
        (p.min_ids(), p.mult(), p.qmax(), p.g(), p.t()),
        (2, 1 << 20, 2047, 20, 4)
    );
    assert_eq!((p.m(), p.max_abs_b1()), (2047, 1));
    let as_i8 = |v: &[u8]| v.iter().map(|&b| b as i8).collect::<Vec<i8>>();
    assert_eq!(as_i8(p.emb()), o.emb);
    assert_eq!(as_i8(p.w1()), o.w1);
    assert_eq!(as_i8(p.w2()), o.w2);
    assert_eq!(as_i8(p.lut()), o.lut);
    assert_eq!((p.b1(), p.b2()), (&o.b1[..], &o.b2[..]));
    let h = p.header();
    let offs: Vec<u32> = Tensor::ALL.iter().map(|t| h[t.fields().0]).collect();
    assert_eq!(offs, [96, 112, 128, 144, 160, 176]);
    assert!(offs.iter().all(|o| o.is_multiple_of(TENSOR_ALIGN)));
    assert_eq!(bytes.len(), 176 + 8);
    assert_eq!(p.bytes(), &bytes[..]);
}

/// Steps 1–8 by hand: sign-and-magnitude truncation toward zero in steps 4 and 5, the clamps, the table and the strict argmax.
#[test]
fn the_steps_follow_the_design_by_hand() {
    let o = small();
    let bytes = o.bytes();
    let p = check(&bytes).expect("valid");
    let (offsets, ids) = csr(&[&[0, 1], &[2, 2, 2], &[3], &[], &[1, 0, 3]]);
    let b = Batch::pack(&p, offsets, ids).expect("valid");
    let out = run_both(&p, &b);
    let want: Vec<u32> = [
        words(&[-250, -145], 1),
        words(&[400, 105], 0),
        words(&[0, 0], 2),
        words(&[0, 0], 2),
        words(&[-400, -95], 1),
    ]
    .concat();
    assert_eq!(out, want);

    let o = identity(3 << 19);
    let bytes = o.bytes();
    let p = check(&bytes).expect("valid");
    assert_eq!((p.qmax(), p.m()), (1365, 2047));
    let (offsets, ids) = csr(&[&[0, 1]]);
    let b = Batch::pack(&p, offsets, ids).expect("valid");
    assert_eq!(
        run_both(&p, &b),
        words(&[-18, -3], 1),
        "−7.5 truncates to −7"
    );
    let t = trace(&p, &b, 0).expect("traced").expect("not abstaining");
    assert_eq!(t.pre, [-6, -4]);
    assert_eq!((t.clamped, t.saturated, t.interior), (0, 0, 2));

    let o = identity(1 << 29);
    let bytes = o.bytes();
    let p = check(&bytes).expect("valid");
    assert_eq!((p.qmax(), p.m()), (3, 1536));
    let (offsets, ids) = csr(&[&[2, 2, 2]]);
    let b = Batch::pack(&p, offsets, ids).expect("valid");
    assert_eq!(run_both(&p, &b), words(&[64, 21], 0));
    let t = trace(&p, &b, 0).expect("traced").expect("not abstaining");
    assert_eq!(t.pre, [1537, 1535], "q clamps to QMAX = 3");
    assert_eq!((t.clamped, t.saturated, t.interior), (2, 2, 0));
}

/// The lowest of tied classes wins, as the shipped strict `>` scan picks it.
#[test]
fn ties_go_to_the_lowest_class() {
    let mut o = small();
    o.dims.classes = 3;
    o.w2 = vec![1, 1, 1, 2, 2, 2];
    o.b2 = vec![7, 7, 7];
    let bytes = o.bytes();
    let p = check(&bytes).expect("valid");
    let (offsets, ids) = csr(&[&[0, 1], &[2, 2]]);
    let b = Batch::pack(&p, offsets, ids).expect("valid");
    let out = run_both(&p, &b);
    assert_eq!(out[3], 0);
    assert_eq!(out[7], 0);
    o.b2 = vec![6, 7, 7];
    let bytes = o.bytes();
    let p = check(&bytes).expect("valid");
    let (offsets, ids) = csr(&[&[0, 1]]);
    let b = Batch::pack(&p, offsets, ids).expect("valid");
    assert_eq!(run_both(&p, &b)[3], 1);
}

/// A batch of only empty rows (TOTAL_IDS = 0) is valid, abstains everywhere and still takes 4 bytes of ids.
#[test]
fn an_all_abstain_batch_with_no_ids_is_valid() {
    let o = small();
    let bytes = o.bytes();
    let p = check(&bytes).expect("valid");
    let b = Batch::pack(&p, vec![0, 0, 0], vec![]).expect("valid");
    assert_eq!((b.rows(), b.total_ids()), (2, 0));
    assert_eq!(run_both(&p, &b), [0, 0, 2, 0, 0, 2]);
    assert_eq!(
        b.bytes(),
        JobBytes {
            job: 20,
            offsets: 12,
            ids: 4,
            out: 24,
        }
    );
    assert_eq!(b.row(0), Some(&[][..]));
    assert_eq!(b.row(2), None);
    assert_eq!(trace(&p, &b, 0), Ok(None));
}

/// Each buffer is counted at `max(len, 4)`, and ids are padded to 4 bytes.
#[test]
fn job_bytes_count_each_buffer_as_it_is_made() {
    assert_eq!(
        JobBytes::of(3, 5, 4),
        JobBytes {
            job: 20,
            offsets: 16,
            ids: 12,
            out: 60,
        }
    );
    assert_eq!(JobBytes::of(3, 5, 4).total(), 108);
    assert_eq!(JobBytes::of(1, 0, 1).ids, 4);
    assert_eq!(JobBytes::of(1, 1, 1).ids, 4);
    assert_eq!(JobBytes::of(1, 2, 1).ids, 4);
    assert_eq!(JobBytes::of(1, 3, 1).ids, 8);
}

/// The batch's header words and accessors are the layout's.
#[test]
fn a_batch_reads_back_its_header() {
    let o = small();
    let (offsets, ids) = csr(&[&[0, 1, 2], &[], &[3]]);
    let b = packed(&o, offsets, ids).expect("valid");
    let h = b.header();
    assert_eq!(h[job::MAGIC], JOB_MAGIC_VALUE);
    assert_eq!(h[job::LAYOUT_VERSION], LAYOUT_VERSION);
    assert_eq!(
        (h[job::ROWS], h[job::TOTAL_IDS], h[job::OUT_STRIDE]),
        (3, 4, 3)
    );
    assert_eq!(b.offsets(), [0, 3, 3, 4]);
    assert_eq!(b.ids(), [0, 1, 2, 3]);
    assert_eq!(b.row(2), Some(&[3][..]));
    assert_eq!(b.out_words(), 9);
    let (offsets, ids) = b.into_parts();
    assert_eq!((offsets.len(), ids.len()), (4, 4));
}

/// forward refuses mismatched dimensions, rows outside the batch and a wrongly sized `out` before computing.
#[test]
fn forward_refuses_its_arguments_by_name() {
    let o = small();
    let (offsets, ids) = csr(&[&[0, 1], &[2, 3]]);
    let b = packed(&o, offsets, ids).expect("valid");
    let mut other = small();
    other.dims.buckets = 5;
    other.emb.extend([1, 1]);
    let bytes = other.bytes();
    let q = check(&bytes).expect("valid");
    let mut out = vec![0u32; 6];
    assert_eq!(
        forward(&q, &b, &mut out),
        Err(ForwardError::DimsMismatch {
            batch: o.dims,
            params: other.dims,
        })
    );
    let bytes = o.bytes();
    let p = check(&bytes).expect("valid");
    assert_eq!(
        forward(&p, &b, &mut out[..5]),
        Err(ForwardError::OutLength { want: 6, got: 5 })
    );
    assert_eq!(
        forward_fast_rows(&p, &b, 1..3, &mut out[..6]),
        Err(ForwardError::RowsOutOfRange {
            start: 1,
            end: 3,
            rows: 2,
        })
    );
    assert_eq!(
        trace(&p, &b, 2),
        Err(ForwardError::RowsOutOfRange {
            start: 2,
            end: 3,
            rows: 2,
        })
    );
    let mut whole = vec![0u32; 6];
    forward_fast(&p, &b, &mut whole).expect("valid");
    let (first, second) = out.split_at_mut(3);
    forward_fast_rows(&p, &b, 0..1, first).expect("valid");
    forward_fast_rows(&p, &b, 1..2, second).expect("valid");
    assert_eq!(out, whole, "row ranges compose");
    forward_fast_rows(&p, &b, 1..1, &mut []).expect("an empty range");
}

/// `out_digest` is `Digest::of(OUT_DOMAIN, [the little-endian words])`.
#[test]
fn out_digest_covers_every_word() {
    let w = [1u32, 0xdead_beef, 5];
    let bytes: Vec<u8> = w.iter().flat_map(|x| x.to_le_bytes()).collect();
    assert_eq!(out_digest(&w), Digest::of(OUT_DOMAIN, &[&bytes]));
    assert_ne!(out_digest(&w), out_digest(&[1, 0xdead_beef, 4]));
}

/// L4: a blob over `PARAMS_BYTES_MAX` is refused.
#[test]
fn refuses_a_blob_too_large() {
    let mut bytes = small().bytes();
    bytes.resize(PARAMS_BYTES_MAX as usize + 1, 0);
    assert_eq!(
        refused(&bytes),
        ParamsError::TooLarge {
            bytes: PARAMS_BYTES_MAX + 1,
            limit: PARAMS_BYTES_MAX,
        }
    );
}

/// L4: a blob shorter than its header is refused.
#[test]
fn refuses_a_truncated_header() {
    let bytes = small().bytes();
    assert_eq!(
        refused(&bytes[..PARAMS_WORDS * 4 - 1]),
        ParamsError::Truncated { bytes: 91 }
    );
}

/// L4: a wrong magic word is refused.
#[test]
fn refuses_bad_magic() {
    let mut bytes = small().bytes();
    bytes[0] ^= 1;
    assert_eq!(
        refused(&bytes),
        ParamsError::BadMagic {
            found: PARAMS_MAGIC_VALUE ^ 1,
        }
    );
}

/// L4: another layout version is refused.
#[test]
fn refuses_another_layout_version() {
    let bytes = patched(&small(), params::LAYOUT_VERSION, 2);
    assert_eq!(refused(&bytes), ParamsError::BadVersion { found: 2 });
}

/// L4: each dimension at 0 and one past its maximum is refused by name.
#[test]
fn refuses_dimensions_outside_their_bounds() {
    for (at, dim, max) in [
        (params::B, Dim::Buckets, B_MAX),
        (params::E, Dim::Embed, E_MAX),
        (params::H, Dim::Hidden, H_MAX),
        (params::C, Dim::Classes, C_MAX),
    ] {
        for value in [0, max + 1] {
            let bytes = patched(&small(), at, value);
            assert_eq!(
                refused(&bytes),
                ParamsError::DimOutOfBounds { dim, value },
                "{dim:?} = {value}"
            );
        }
    }
}

/// L4: MIN_IDS = 0 is refused.
#[test]
fn refuses_min_ids_zero() {
    let o = Owned {
        min_ids: 0,
        ..small()
    };
    assert_eq!(refused(&o.bytes()), ParamsError::MinIdsZero);
}

/// L4: G outside 1..=31 is refused.
#[test]
fn refuses_a_shift_outside_its_range() {
    for g in [0, 32] {
        let o = Owned { g, ..small() };
        assert_eq!(refused(&o.bytes()), ParamsError::ShiftOutOfRange { g });
    }
}

/// L4: a negative T is refused.
#[test]
fn refuses_a_negative_t() {
    let o = Owned { t: -1, ..small() };
    assert_eq!(refused(&o.bytes()), ParamsError::NegativeT { t: -1 });
}

/// L4: MULT = 0 and MULT = 2³¹ are refused.
#[test]
fn refuses_mult_outside_its_range() {
    for mult in [0, 1 << 31] {
        let o = Owned { mult, ..small() };
        assert_eq!(refused(&o.bytes()), ParamsError::MultOutOfRange { mult });
    }
    let o = Owned {
        mult: i32::MAX as u32,
        g: 31,
        t: 0,
        lut: vec![0],
        b1: vec![0, 0],
        ..small()
    };
    assert!(check(&o.bytes()).is_ok(), "2³¹ − 1 is in range");
}

/// L4: a stored QMAX that is not recomputed from MULT is refused.
#[test]
fn refuses_a_stored_qmax_that_is_not_recomputed() {
    let bytes = patched(&small(), params::QMAX, 2046);
    assert_eq!(
        refused(&bytes),
        ParamsError::QmaxMismatch {
            stored: 2046,
            want: 2047,
        }
    );
}

/// L4: each tensor one element too long is refused by name.
#[test]
fn refuses_a_tensor_of_the_wrong_length() {
    for (tensor, want) in [
        (Tensor::Emb, 8),
        (Tensor::W1, 4),
        (Tensor::W2, 4),
        (Tensor::Lut, 9),
        (Tensor::B1, 2),
        (Tensor::B2, 2),
    ] {
        let mut o = small();
        match tensor {
            Tensor::Emb => o.emb.push(0),
            Tensor::W1 => o.w1.push(0),
            Tensor::W2 => o.w2.push(0),
            Tensor::Lut => o.lut.push(0),
            Tensor::B1 => o.b1.push(0),
            Tensor::B2 => o.b2.push(0),
        }
        assert_eq!(
            refused(&o.bytes()),
            ParamsError::BadLength {
                tensor,
                len: want as u32 + 1,
                want,
            }
        );
    }
}

/// L4: a tensor ending past the blob is refused.
#[test]
fn refuses_a_tensor_outside_the_blob() {
    let bytes = patched(&small(), params::B2_OFF, 192);
    assert_eq!(
        refused(&bytes),
        ParamsError::OutsideBlob {
            tensor: Tensor::B2,
            offset: 192,
            end: 200,
        }
    );
}

/// L4: a tensor offset that is not 16-byte aligned is refused.
#[test]
fn refuses_an_unaligned_tensor() {
    let bytes = patched(&small(), params::EMB_OFF, 97);
    assert_eq!(
        refused(&bytes),
        ParamsError::Unaligned {
            tensor: Tensor::Emb,
            offset: 97,
        }
    );
}

/// L4: a tensor over the header or over another tensor is refused.
#[test]
fn refuses_overlapping_tensors() {
    let bytes = patched(&small(), params::EMB_OFF, 80);
    assert_eq!(
        refused(&bytes),
        ParamsError::Overlap {
            tensor: Tensor::Emb,
        }
    );
    let bytes = patched(&small(), params::W1_OFF, 96);
    assert_eq!(refused(&bytes), ParamsError::Overlap { tensor: Tensor::W1 });
}

/// L4: bytes after the last tensor are refused.
#[test]
fn refuses_trailing_bytes() {
    let mut bytes = small().bytes();
    bytes.extend([0; 16]);
    assert_eq!(refused(&bytes), ParamsError::Trailing { bytes: 16 });
}

/// L4: −128 in each i8 tensor is refused by name.
#[test]
fn refuses_minus_128_in_each_i8_tensor() {
    for tensor in [Tensor::Emb, Tensor::W1, Tensor::W2, Tensor::Lut] {
        let mut o = small();
        match tensor {
            Tensor::Emb => o.emb[5] = -128,
            Tensor::W1 => o.w1[3] = -128,
            Tensor::W2 => o.w2[1] = -128,
            _ => o.lut[0] = -128,
        }
        let index = match tensor {
            Tensor::Emb => 5,
            Tensor::W1 => 3,
            Tensor::W2 => 1,
            _ => 0,
        };
        assert_eq!(refused(&o.bytes()), ParamsError::Int8Min { tensor, index });
    }
}

/// L4: `M − max|b1| < T` is refused, and the boundary `M − max|b1| = T` accepted.
#[test]
fn refuses_a_lossy_clamp() {
    let o = Owned {
        b1: vec![2044, 0],
        ..small()
    };
    assert_eq!(
        refused(&o.bytes()),
        ParamsError::Lossy {
            m: 2047,
            max_b1: 2044,
            t: 4,
        }
    );
    let o = Owned {
        b1: vec![0, -2043],
        ..small()
    };
    assert!(check(&o.bytes()).is_ok(), "2047 − 2043 = T");
    let o = Owned {
        b1: vec![i32::MIN, 0],
        ..small()
    };
    assert_eq!(
        refused(&o.bytes()),
        ParamsError::Lossy {
            m: 2047,
            max_b1: 1 << 31,
            t: 4,
        },
        "|i32::MIN| is computed in i64"
    );
}

/// L4: `|b2[c]| ≥ 2²⁹` is refused, and `2²⁹ − 1` accepted.
#[test]
fn refuses_b2_at_two_to_the_29() {
    for value in [B2_LIMIT, -B2_LIMIT] {
        let o = Owned {
            b2: vec![0, value],
            ..small()
        };
        assert_eq!(
            refused(&o.bytes()),
            ParamsError::B2OutOfRange { class: 1, value }
        );
    }
    let o = Owned {
        b2: vec![B2_LIMIT - 1, 1 - B2_LIMIT],
        ..small()
    };
    assert!(check(&o.bytes()).is_ok());
}

/// L4: a batch with no rows is `Empty`.
#[test]
fn refuses_an_empty_batch() {
    assert_eq!(packed(&small(), vec![0], vec![]), Err(BatchError::Empty));
    assert_eq!(packed(&small(), vec![], vec![]), Err(BatchError::Empty));
}

/// L4: decreasing offsets are `NotMonotone`.
#[test]
fn refuses_offsets_that_decrease() {
    assert_eq!(
        packed(&small(), vec![0, 2, 1, 3], vec![0, 1, 2]),
        Err(BatchError::NotMonotone { row: 1 })
    );
}

/// L4: offsets that do not span the ids exactly are `BadTotal`.
#[test]
fn refuses_offsets_that_do_not_span_the_ids() {
    assert_eq!(
        packed(&small(), vec![0, 2], vec![0, 1, 2]),
        Err(BatchError::BadTotal {
            first: 0,
            last: 2,
            ids: 3,
        })
    );
    assert_eq!(
        packed(&small(), vec![1, 3], vec![0, 1, 2]),
        Err(BatchError::BadTotal {
            first: 1,
            last: 3,
            ids: 3,
        })
    );
}

/// L4: an id not below B is `IdOutOfRange`.
#[test]
fn refuses_an_id_out_of_range() {
    assert_eq!(
        packed(&small(), vec![0, 3], vec![0, 4, 1]),
        Err(BatchError::IdOutOfRange {
            at: 1,
            id: 4,
            buckets: 4,
        })
    );
}

/// L4: a row over `MAX_ROW_IDS` is `RowTooLong`, and one at it is valid.
#[test]
fn refuses_a_row_too_long() {
    let n = MAX_ROW_IDS + 1;
    assert_eq!(
        packed(&small(), vec![0, 0, n], vec![1; n as usize]),
        Err(BatchError::RowTooLong { row: 1, len: n })
    );
    assert!(packed(&small(), vec![0, n - 1], vec![1; n as usize - 1]).is_ok());
}

/// L4: more than `ROWS_MAX` rows is `TooManyRows`, and `ROWS_MAX` is valid.
#[test]
fn refuses_too_many_rows() {
    let mut o = small();
    o.dims.classes = 1;
    o.w2.truncate(2);
    o.b2.truncate(1);
    let rows = ROWS_MAX as usize;
    assert_eq!(
        packed(&o, vec![0; rows + 2], vec![]),
        Err(BatchError::TooManyRows {
            rows: rows as u64 + 1,
            limit: ROWS_MAX,
        })
    );
    assert!(packed(&o, vec![0; rows + 1], vec![]).is_ok());
}

/// L4: a batch whose buffers would pass `JOB_BYTES_MAX` is `TooLarge`.
#[test]
fn refuses_a_batch_too_large() {
    let mut o = small();
    o.dims.classes = C_MAX;
    o.w2 = vec![1; 2 * C_MAX as usize];
    o.b2 = vec![0; C_MAX as usize];
    let rows = ROWS_MAX as u64;
    let bytes = JobBytes::of(rows, 0, C_MAX).total();
    assert_eq!(bytes, 20 + (rows + 1) * 4 + 4 + rows * 17 * 4);
    assert_eq!(
        packed(&o, vec![0; rows as usize + 1], vec![]),
        Err(BatchError::TooLarge {
            bytes,
            limit: JOB_BYTES_MAX,
        })
    );
}

/// T3: checked forward runs over the extreme blob at `MAX_ROW_IDS` without panicking, reaches both clamps, and breaks every tie toward the lower class.
#[test]
fn checked_forward_holds_at_the_extremes() {
    let bytes = extreme_params();
    let p = check(&bytes).expect("the extreme blob is within bounds");
    assert_eq!(
        p.dims(),
        Dims {
            buckets: B_MAX,
            embed: E_MAX,
            hidden: H_MAX,
            classes: C_MAX,
        }
    );
    assert_eq!(
        i64::from(p.m()) - i64::from(p.max_abs_b1()),
        i64::from(p.t())
    );
    assert!(p.b2().iter().all(|&x| x.abs() == B2_LIMIT - 1));
    for (a, b) in [(p.emb(), 127), (p.w1(), 127), (p.w2(), 127)] {
        assert!(a.iter().all(|&x| (x as i8).abs() == b), "every weight ±127");
    }
    let (offsets, ids) = extreme_rows();
    let b = Batch::pack(&p, offsets, ids).expect("within bounds");
    let out = run_both(&p, &b);
    let stride = b.out_stride() as usize;
    let mut classes = BTreeSet::new();
    let (mut clamped, mut saturated, mut unsaturated, mut full) = (0, 0, 0, 0);
    for row in 0..b.rows() {
        let class = out[row as usize * stride + stride - 1];
        full += usize::from(b.row(row).map(<[u16]>::len) == Some(MAX_ROW_IDS as usize));
        match trace(&p, &b, row).expect("traced") {
            None => assert_eq!(class, C_MAX),
            Some(t) => {
                assert!([0, 4, 8, 12].contains(&class), "row {row}: {class}");
                classes.insert(class);
                clamped += usize::from(t.clamped > 0);
                saturated += usize::from(t.saturated > 0);
                unsaturated += usize::from(t.saturated == 0);
            }
        }
    }
    let m = p.m() as i32;
    let first = trace(&p, &b, 0).expect("traced").expect("row 0");
    assert_eq!(
        first.pre[0],
        m + p.b1()[0],
        "step 3's bound, clamped to QMAX"
    );
    let second = trace(&p, &b, 1).expect("traced").expect("row 1");
    assert_eq!(second.pre[0], p.b1()[0] - m, "and its negative");
    assert!(first.clamped > 0 && first.saturated > 0);
    assert_eq!(full, 3);
    assert!(clamped >= 2 && saturated >= 2 && unsaturated >= 1);
    assert!(classes.len() >= 2, "{classes:?}");
}

/// Whether `p`'s activation table is the sign function, which hides every step-4–6 detail but the sign.
fn sign_lut(p: &ParamsView<'_>) -> bool {
    p.lut()
        .iter()
        .zip(-p.t()..)
        .all(|(&v, i)| i32::from(v as i8) == i.signum() * 127)
}

/// T3: forward_fast equals forward word for word over 64 seeded blobs within bounds, which predict at least four classes, reach the row cap and both clamps, and hold interior units under a table that is not the sign function.
#[test]
fn forward_fast_equals_forward_on_seeded_blobs() {
    let mut classes = BTreeSet::new();
    let (mut clamped, mut saturated, mut full, mut abstained, mut interior) = (0, 0, 0, 0, 0);
    for seed in 0..64 {
        let (bytes, offsets, ids) = seeded(seed);
        let p = check(&bytes).unwrap_or_else(|e| panic!("seed {seed}: {e:?}"));
        let b = Batch::pack(&p, offsets, ids).unwrap_or_else(|e| panic!("seed {seed}: {e:?}"));
        let out = run_both(&p, &b);
        let stride = b.out_stride() as usize;
        let sign = sign_lut(&p);
        for row in 0..b.rows() {
            full += usize::from(b.row(row).map(<[u16]>::len) == Some(MAX_ROW_IDS as usize));
            match trace(&p, &b, row).expect("traced") {
                None => abstained += 1,
                Some(t) => {
                    classes.insert(out[row as usize * stride + stride - 1]);
                    clamped += usize::from(t.clamped > 0);
                    saturated += usize::from(t.saturated > 0);
                    interior += if sign { 0 } else { t.interior };
                }
            }
        }
    }
    assert!(classes.len() >= 4, "{classes:?}");
    assert!(clamped > 0 && saturated > 0 && abstained > 0);
    assert_eq!(full, 8);
    assert!(
        interior >= 10_000,
        "{interior} interior units under non-sign tables, below the 10,000 floor"
    );
}

/// The interior blob is within bounds at the profile's smallest, tone's and largest shapes, and its rows reach neither clamp while more than half their units are interior.
#[test]
fn the_interior_blob_stays_unclamped_at_every_shape() {
    for (embed, hidden, classes, min_ids) in [(1, 1, 1, 1), (32, 64, 5, 6), (32, 128, 16, 2)] {
        let dims = Dims {
            buckets: 256,
            embed,
            hidden,
            classes,
        };
        let bytes = interior_params_for(dims, min_ids);
        let p = check(&bytes).unwrap_or_else(|e| panic!("{dims:?}: {e:?}"));
        assert!(!sign_lut(&p));
        assert_eq!((p.mult(), p.g(), p.t()), (181, 8, 127));
        let (offsets, ids) = interior_rows(dims, min_ids);
        let b = Batch::pack(&p, offsets, ids).expect("within bounds");
        assert_eq!(b.rows(), 64);
        run_both(&p, &b);
        let (mut units, mut interior) = (0, 0);
        for row in 0..b.rows() {
            let t = trace(&p, &b, row)
                .expect("traced")
                .expect("no row abstains");
            assert_eq!((t.clamped, t.saturated), (0, 0), "{dims:?} row {row}");
            units += hidden;
            interior += t.interior;
        }
        assert!(2 * interior > units, "{dims:?}: {interior} of {units}");
    }
}
