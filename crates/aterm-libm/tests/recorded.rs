// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Recorded differential tests for all 68 functions against `recorded/libm-0.2.16.tsv`.

use std::collections::BTreeSet;

const RECORDED: &str = include_str!("recorded/libm-0.2.16.tsv");

/// Exactly specified IEEE operations: one correct answer per input.
const EXACT: &[&str] = &[
    "floor",
    "floorf",
    "ceil",
    "ceilf",
    "round",
    "roundf",
    "trunc",
    "truncf",
    "fabs",
    "fabsf",
    "rint",
    "rintf",
    "copysign",
    "copysignf",
    "sqrt",
    "sqrtf",
    "fma",
    "fmaf",
    "fdim",
    "fdimf",
];

/// How far a transcendental may stand from musl's recorded answer. Measured
/// on aarch64-apple-darwin at the switch: 95 of the 2,762 rows differ, each
/// by exactly one ulp. Two leaves room for another platform's libm, which is
/// also within an ulp of the true value.
const ULPS: u64 = 2;

/// One recorded row: the function, its argument bits, its result bits.
struct Row<'a> {
    name: &'a str,
    args: Vec<u64>,
    want: Vec<u64>,
}

fn rows() -> Vec<Row<'static>> {
    let hex = |s: &str| {
        s.split(',')
            .map(|h| u64::from_str_radix(h, 16).expect("hex bits"))
            .collect::<Vec<_>>()
    };
    RECORDED
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            assert_eq!(f.len(), 3, "malformed row: {l}");
            Row {
                name: f[0],
                args: hex(f[1]),
                want: hex(f[2]),
            }
        })
        .collect()
}

/// Calls the shim's function `name` on recorded argument bits; the result
/// bits come back widened to `u64`. `None` names no function of the shim.
fn call(name: &str, a: &[u64]) -> Option<Vec<u64>> {
    let d = |i: usize| f64::from_bits(a[i]);
    let s = |i: usize| f32::from_bits(a[i] as u32);
    let w = |x: f32| u64::from(x.to_bits());
    macro_rules! table {
        ($($u64:ident)*; $($b64:ident)*; $($u32:ident)*; $($b32:ident)*) => {
            $(if name == stringify!($u64) { return Some(vec![libm::$u64(d(0)).to_bits()]); })*
            $(if name == stringify!($b64) { return Some(vec![libm::$b64(d(0), d(1)).to_bits()]); })*
            $(if name == stringify!($u32) { return Some(vec![w(libm::$u32(s(0)))]); })*
            $(if name == stringify!($b32) { return Some(vec![w(libm::$b32(s(0), s(1)))]); })*
        };
    }
    table!(
        floor ceil round trunc fabs rint sqrt exp exp2 log log2 log10 cbrt sin cos tan asin
        acos atan expm1 log1p sinh cosh tanh asinh acosh atanh;
        fdim pow hypot atan2 copysign;
        floorf ceilf roundf truncf fabsf rintf sqrtf expf exp2f logf log2f log10f cbrtf sinf
        cosf tanf asinf acosf atanf expm1f log1pf sinhf coshf tanhf asinhf acoshf atanhf;
        fdimf powf hypotf atan2f copysignf
    );
    match name {
        "sincos" => {
            let (sn, cs) = libm::sincos(d(0));
            Some(vec![sn.to_bits(), cs.to_bits()])
        }
        "sincosf" => {
            let (sn, cs) = libm::sincosf(s(0));
            Some(vec![w(sn), w(cs)])
        }
        "fma" => Some(vec![libm::fma(d(0), d(1), d(2)).to_bits()]),
        "fmaf" => Some(vec![w(libm::fmaf(s(0), s(1), s(2)))]),
        _ => None,
    }
}

/// Whether `got` stands close enough to `want` for function `name`.
fn agrees(name: &str, want: u64, got: u64) -> bool {
    let f32_row = name.ends_with('f');
    let nan = |b: u64| {
        if f32_row {
            f32::from_bits(b as u32).is_nan()
        } else {
            f64::from_bits(b).is_nan()
        }
    };
    let ordered = |b: u64| {
        let (i, min) = if f32_row {
            (i64::from(b as u32 as i32), i64::from(i32::MIN))
        } else {
            (b as i64, i64::MIN)
        };
        if i < 0 { min - i } else { i }
    };
    if nan(want) || nan(got) {
        return nan(want) && nan(got);
    }
    if EXACT.contains(&name) {
        return want == got;
    }
    ordered(want).abs_diff(ordered(got)) <= ULPS
}

/// The rows of `recorded` that `called` misses.
fn mismatches(recorded: &str, called: &str) -> Vec<String> {
    rows()
        .iter()
        .filter(|r| r.name == recorded)
        .filter_map(|r| {
            let got = call(called, &r.args).unwrap_or_else(|| panic!("no function {called}"));
            let ok = r.want.len() == got.len()
                && r.want
                    .iter()
                    .zip(&got)
                    .all(|(w, g)| agrees(recorded, *w, *g));
            (!ok).then(|| {
                format!(
                    "{recorded}({:x?}) = {got:x?}, recorded {:x?}",
                    r.args, r.want
                )
            })
        })
        .collect()
}

#[test]
fn every_function_matches_the_recorded_fork() {
    let names: BTreeSet<&str> = rows().iter().map(|r| r.name).collect();
    assert_eq!(names.len(), 68, "recorded functions: {names:?}");
    let mut bad = Vec::new();
    for name in &names {
        bad.extend(mismatches(name, name));
    }
    assert!(
        bad.is_empty(),
        "{} rows disagree:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

#[test]
fn inverse_hyperbolic_tangent_keeps_zero_sign_and_domain_boundaries() {
    macro_rules! check {
        ($float:ty, $atanh:path) => {
            for x in [0.0 as $float, -0.0 as $float] {
                assert_eq!($atanh(x).to_bits(), x.to_bits());
            }
            for x in [1.0 as $float, -1.0 as $float] {
                assert_eq!($atanh(x), <$float>::INFINITY.copysign(x));
            }
            for x in [
                1.5 as $float,
                -1.5 as $float,
                <$float>::INFINITY,
                <$float>::NEG_INFINITY,
                <$float>::NAN,
            ] {
                assert!($atanh(x).is_nan());
            }
        };
    }
    check!(f64, libm::atanh);
    check!(f32, libm::atanhf);
}
