// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! X25519 (RFC 7748 §5) scalar multiplication, and the Elligator 2 map
//! onto curve25519 (RFC 9380 §6.7.1) that CPace's generator needs ([`elligator2`]).
//!
//! The field is GF(2^255 - 19) in radix 2^51: five `u64` limbs whose products are `u128`. Its
//! product, square, carry, sum, difference and reduction to bytes follow fiat-crypto's
//! `curve25519_64` (Erbsen, Philipoom, Gross, Sloan and Chlipala, S&P 2019), the field code ring
//! and BoringSSL run; fiat-crypto's Coq proof covers that generated code and its limb bounds, not
//! this Rust. Two types hold the bounds, so the compiler refuses a sum of sums and no limb or
//! column can overflow: [`Fe`], each limb at most 2^51, and [`Loose`], each at most 3 * 2^51. The
//! ladder is RFC 7748 §5's pseudocode, line for line, and inversion is ref10's addition chain.
//! It replaced TweetNaCl's sixteen 16-bit limbs (Schwabe, Viguier, Weerwag and Wiedijk proved that
//! C correct in Coq, CSF 2021), which the tests keep as an oracle that every output must match.
//!
//! The field arithmetic, the ladder and the map run in constant time in their secret inputs: no
//! branch, loop bound or memory index depends on a field element or a scalar bit. Loops run over
//! fixed limb counts and public exponents, and choices are made by masks ([`cswap`], [`select`],
//! [`equal`], [`Fe::to_bytes`]), each behind `black_box`, so the optimizer cannot learn that a
//! mask is all ones or zero and turn it back into a branch. [`canonical_u`], which checks a public
//! key, is the one function that compares by value.

use zeroize::Zeroize;

/// The u-coordinate 9, the base point.
pub const BASEPOINT: [u8; 32] = {
    let mut point = [0u8; 32];
    point[0] = 9;
    point
};

/// X25519(scalar, u): the u-coordinate of the clamped scalar times the point with u-coordinate `u`.
#[inline]
#[must_use]
pub fn x25519(scalar: &[u8; 32], u: &[u8; 32]) -> [u8; 32] {
    ladder(scalar, u)
}

/// X25519(scalar, 9): the public key of `scalar`.
#[inline]
#[must_use]
pub fn x25519_base(scalar: &[u8; 32]) -> [u8; 32] {
    x25519(scalar, &BASEPOINT)
}

/// Whether the little-endian u-coordinate is below 2^255 - 19. X25519 accepts
/// other encodings too, but identity bindings must not treat those as new keys.
/// This checks encoding only, not whether a point has low order.
pub(crate) fn canonical_u(point: &[u8; 32]) -> bool {
    const MODULUS: [u8; 32] = {
        let mut p = [0xff; 32];
        p[0] = 0xed;
        p[31] = 0x7f;
        p
    };
    point.iter().rev().cmp(MODULUS.iter().rev()).is_lt()
}

/// The low 51 bits of a limb.
const LOW_51: u64 = (1 << 51) - 1;

/// p = 2^255 - 19, limb by limb.
const P: [u64; 5] = [LOW_51 - 18, LOW_51, LOW_51, LOW_51, LOW_51];

/// An element of GF(p) as five radix-2^51 limbs (its value is the sum of limb i times
/// 2^(51 i)), each at most 2^51: fiat-crypto's tight bounds. A product, a square, [`carry`] and
/// [`Fe::from_bytes`] give one; [`add`], [`sub`] and [`Fe::to_bytes`] read one.
#[derive(Clone, Copy)]
struct Fe([u64; 5]);

/// A sum or difference of two [`Fe`], each limb at most 3 * 2^51: fiat-crypto's loose bounds.
/// Only a product, a square and [`carry`] read one, so nothing is added to it before a carry.
#[derive(Clone, Copy)]
struct Loose([u64; 5]);

/// The limbs a product, a square or a carry reads: an [`Fe`] or a [`Loose`].
trait Limbs {
    fn limbs(&self) -> [u64; 5];
}

impl Limbs for Fe {
    #[inline]
    fn limbs(&self) -> [u64; 5] {
        self.0
    }
}

impl Limbs for Loose {
    #[inline]
    fn limbs(&self) -> [u64; 5] {
        self.0
    }
}

impl Zeroize for Fe {
    fn zeroize(&mut self) {
        self.0.zeroize();
    }
}

impl Zeroize for Loose {
    fn zeroize(&mut self) {
        self.0.zeroize();
    }
}

const ZERO: Fe = Fe([0; 5]);

const ONE: Fe = Fe([1, 0, 0, 0, 0]);

/// curve25519's Montgomery coefficient `A` = 486662 (RFC 7748 §4.1).
const A: Fe = Fe([486_662, 0, 0, 0, 0]);

/// RFC 7748 §5's `a24` = (486662 - 2) / 4 = 121665.
const A24: u64 = 121_665;

impl Fe {
    /// The element a little-endian u-coordinate names, read as RFC 7748's `decodeUCoordinate`
    /// reads 255 bits: bit 255 is dropped, and a value at or above p stays unreduced (each limb
    /// still below 2^51), which the arithmetic takes mod p.
    fn from_bytes(bytes: &[u8; 32]) -> Self {
        let mut w = [0u64; 4];
        for (word, chunk) in w.iter_mut().zip(bytes.as_chunks::<8>().0) {
            *word = u64::from_le_bytes(*chunk);
        }
        w[3] &= u64::MAX >> 1;
        Self([
            w[0] & LOW_51,
            ((w[0] >> 51) | (w[1] << 13)) & LOW_51,
            ((w[1] >> 38) | (w[2] << 26)) & LOW_51,
            ((w[2] >> 25) | (w[3] << 39)) & LOW_51,
            w[3] >> 12,
        ])
    }

    /// The canonical little-endian bytes, the value reduced below p. With every limb at most
    /// 2^51 the value is below 2p, so it is `h` or `h - p`: fiat-crypto's `to_bytes` subtracts p
    /// with 51-bit borrows and adds p back, chosen by a mask from the last borrow.
    fn to_bytes(self) -> [u8; 32] {
        let mut t = [0u64; 5];
        let mut borrow = 0u64;
        for i in 0..5 {
            // At most 19 above zero, so the low 51 bits lose nothing, and bit 63 is set exactly
            // when the difference went below zero.
            let d = self.0[i].wrapping_sub(P[i]).wrapping_sub(borrow);
            t[i] = d & LOW_51;
            borrow = d >> 63;
        }
        let mask = std::hint::black_box(0u64.wrapping_sub(borrow));
        let mut carry = 0u64;
        for i in 0..5 {
            let s = t[i] + (P[i] & mask) + carry;
            t[i] = s & LOW_51;
            carry = s >> 51;
        }
        // The carry out of limb 4 is the 2^255 the borrow took, so it is dropped.
        let words = [
            t[0] | (t[1] << 51),
            (t[1] >> 13) | (t[2] << 38),
            (t[2] >> 26) | (t[3] << 25),
            (t[3] >> 39) | (t[4] << 12),
        ];
        let mut out = [0u8; 32];
        for (chunk, word) in out.as_chunks_mut::<8>().0.iter_mut().zip(words) {
            *chunk = word.to_le_bytes();
        }
        t.zeroize();
        out
    }
}

/// `a * b` as a `u128`.
#[inline]
fn wide(a: u64, b: u64) -> u128 {
    u128::from(a) * u128::from(b)
}

/// Five column sums carried into limbs along fiat-crypto's chain 0, 1, 2, 3, 4, 0, 1, 2:
/// what passes 2^255 comes back times 19, as 2^255 = 19 (mod p). For the columns of a product
/// of two [`Loose`] (each below 2^113, column 4 below 2^108) the carry out of limb 4 is below
/// 2^58, so its 19-fold fits a `u64`, and every limb ends at most 2^51.
#[inline]
fn carry_columns(c: [u128; 5]) -> Fe {
    let mut l = [0u64; 5];
    let mut carry = 0u128;
    for i in 0..5 {
        let column = c[i] + carry;
        l[i] = (column as u64) & LOW_51;
        carry = column >> 51;
    }
    let low = l[0] + 19 * (carry as u64);
    l[0] = low & LOW_51;
    let next = l[1] + (low >> 51);
    l[1] = next & LOW_51;
    l[2] += next >> 51;
    Fe(l)
}

/// `a * b` (fiat-crypto's `carry_mul`). A limb `i + j` at or past 5 folds into `i + j - 5` times
/// 19, so each `b_j` that lands there is taken times 19 first (below 2^57).
#[inline]
fn mul(a: &impl Limbs, b: &impl Limbs) -> Fe {
    let [a0, a1, a2, a3, a4] = a.limbs();
    let [b0, b1, b2, b3, b4] = b.limbs();
    let (b1_19, b2_19, b3_19, b4_19) = (19 * b1, 19 * b2, 19 * b3, 19 * b4);
    carry_columns([
        wide(a0, b0) + wide(a1, b4_19) + wide(a2, b3_19) + wide(a3, b2_19) + wide(a4, b1_19),
        wide(a0, b1) + wide(a1, b0) + wide(a2, b4_19) + wide(a3, b3_19) + wide(a4, b2_19),
        wide(a0, b2) + wide(a1, b1) + wide(a2, b0) + wide(a3, b4_19) + wide(a4, b3_19),
        wide(a0, b3) + wide(a1, b2) + wide(a2, b1) + wide(a3, b0) + wide(a4, b4_19),
        wide(a0, b4) + wide(a1, b3) + wide(a2, b2) + wide(a3, b1) + wide(a4, b0),
    ])
}

/// `a * a` (fiat-crypto's `carry_square`): each cross product once, doubled.
#[inline]
fn sqr(a: &impl Limbs) -> Fe {
    let [a0, a1, a2, a3, a4] = a.limbs();
    let (a3_19, a4_19) = (19 * a3, 19 * a4);
    let (a3_38, a4_38) = (2 * a3_19, 2 * a4_19);
    carry_columns([
        wide(a0, a0) + wide(a1, a4_38) + wide(a2, a3_38),
        wide(a0, 2 * a1) + wide(a2, a4_38) + wide(a3, a3_19),
        wide(a0, 2 * a2) + wide(a1, a1) + wide(a3, a4_38),
        wide(a0, 2 * a3) + wide(a1, 2 * a2) + wide(a4, a4_19),
        wide(a0, 2 * a4) + wide(a1, 2 * a3) + wide(a2, a2),
    ])
}

/// `a24 * a` (fiat-crypto's `carry_scmul_121666`, with RFC 7748's 121665).
#[inline]
fn mul_a24(a: &impl Limbs) -> Fe {
    carry_columns(a.limbs().map(|limb| wide(A24, limb)))
}

/// `a`, its limbs carried back to at most 2^51 (fiat-crypto's `carry`).
#[inline]
fn carry(a: &Loose) -> Fe {
    carry_columns(a.0.map(u128::from))
}

/// `a + b` (fiat-crypto's `add`): each limb at most 2^52.
#[inline]
fn add(a: &Fe, b: &Fe) -> Loose {
    Loose(std::array::from_fn(|i| a.0[i] + b.0[i]))
}

/// `a - b` (fiat-crypto's `sub`): `2p + a - b` limb by limb, so no limb goes below zero (2p's
/// limbs are at least 2^52 - 38, each `b_i` at most 2^51) and each stays below 3 * 2^51.
#[inline]
fn sub(a: &Fe, b: &Fe) -> Loose {
    let mut o = [0u64; 5];
    for i in 0..5 {
        o[i] = 2 * P[i] + a.0[i] - b.0[i];
    }
    Loose(o)
}

/// Swaps `p` and `q` when `bit` is 1 and leaves them when it is 0, by a mask, never a branch.
/// `bit` must be 0 or 1.
#[inline]
fn cswap(p: &mut Fe, q: &mut Fe, bit: u64) {
    let mask = std::hint::black_box(0u64.wrapping_sub(bit));
    for i in 0..5 {
        let t = mask & (p.0[i] ^ q.0[i]);
        p.0[i] ^= t;
        q.0[i] ^= t;
    }
}

/// `a` when `bit` is 0 and `b` when it is 1, chosen by a mask, never a branch. `bit` must be 0
/// or 1.
#[inline]
fn select(a: &Fe, b: &Fe, bit: u64) -> Fe {
    let mask = std::hint::black_box(0u64.wrapping_sub(bit));
    Fe(std::array::from_fn(|i| a.0[i] ^ (mask & (a.0[i] ^ b.0[i]))))
}

/// 1 when `a` and `b` are the same element of GF(p), whatever their limbs, else 0. Both are
/// reduced to their canonical bytes and every byte pair is folded with no early exit.
fn equal(a: &Fe, b: &Fe) -> u64 {
    let mut x = a.to_bytes();
    let mut y = b.to_bytes();
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= x[i] ^ y[i];
    }
    x.zeroize();
    y.zeroize();
    // `diff - 1` wraps to all ones, so bit 8 is set, exactly when `diff` is 0.
    std::hint::black_box((u64::from(std::hint::black_box(diff)).wrapping_sub(1) >> 8) & 1)
}

/// `z^(2^k)`: `k` squarings, `k` public.
fn sqr_n(z: &Fe, k: u32) -> Fe {
    let mut t = *z;
    for _ in 0..k {
        t = sqr(&t);
    }
    t
}

/// `z^(2^250 - 1)` and `z^11`, the head that ref10's addition chains for p - 2 and (p - 1)/2
/// share: 249 squarings and 10 products, whose counts are fixed.
fn pow_2_250_minus_1(z: &impl Limbs) -> (Fe, Fe) {
    let z2 = sqr(z);
    let z9 = mul(&sqr_n(&z2, 2), z);
    let z11 = mul(&z9, &z2);
    let z_5 = mul(&sqr(&z11), &z9);
    let z_10 = mul(&sqr_n(&z_5, 5), &z_5);
    let z_20 = mul(&sqr_n(&z_10, 10), &z_10);
    let z_40 = mul(&sqr_n(&z_20, 20), &z_20);
    let z_50 = mul(&sqr_n(&z_40, 10), &z_10);
    let z_100 = mul(&sqr_n(&z_50, 50), &z_50);
    let z_200 = mul(&sqr_n(&z_100, 100), &z_100);
    let z_250 = mul(&sqr_n(&z_200, 50), &z_50);
    (z_250, z11)
}

/// `z^(p-2)`, which is `1/z` for a nonzero `z` and 0 for 0: (2^250 - 1) * 2^5 + 11 = 2^255 - 21.
fn invert(z: &impl Limbs) -> Fe {
    let (z_250, z11) = pow_2_250_minus_1(z);
    mul(&sqr_n(&z_250, 5), &z11)
}

/// `z^((p-1)/2)`, Euler's criterion: 1 for a nonzero square, -1 for a non-square, 0 for 0.
/// ((2^250 - 1) * 4 + 1) * 4 + 2 = 2^254 - 10 = (p-1)/2, a chain as fixed as [`invert`]'s.
fn legendre(z: &Fe) -> Fe {
    let (z_250, _) = pow_2_250_minus_1(z);
    mul(&sqr_n(&mul(&sqr_n(&z_250, 2), z), 2), &sqr(z))
}

/// RFC 9380 §4's `is_square`: 1 when `z` is a square in GF(p), zero included, else 0.
fn is_square(z: &Fe) -> u64 {
    let mut l = legendre(z);
    let square = equal(&l, &ONE) | equal(&l, &ZERO);
    l.zeroize();
    square
}

/// `x^3 + A x^2 + x`, the right side of curve25519's equation.
fn curve(x: &Fe) -> Fe {
    mul(&add(&mul(&add(x, &A), x), &ONE), x)
}

/// The u-coordinate of RFC 9380's `map_to_curve_elligator2` for curve25519 (§6.7.1, `Z = 2`,
/// `J = A`, `K = 1`) at `r`, read as RFC 7748's `decodeUCoordinate` reads 255 bits: bit 255 is
/// dropped and a value at or above p is reduced. This is draft-irtf-cfrg-cpace-21's
/// `elligator2` (its Appendix A.5), whose v-coordinate CPace discards, so no square root is taken.
///
/// `1 + 2r^2` is never 0: -1 is a square mod p and 2 is not, so `-1/2` is not, and RFC 9380's
/// exceptional case cannot occur for curve25519.
#[must_use]
pub fn elligator2(r: &[u8; 32]) -> [u8; 32] {
    let mut u = Fe::from_bytes(r);
    let mut u2 = sqr(&u);
    // Z * u^2, carried so that 1 can be added to it.
    let mut tv1 = carry(&add(&u2, &u2));
    // x1 = -A / (1 + Z * u^2)
    let mut x1 = mul(&sub(&ZERO, &A), &invert(&add(&tv1, &ONE)));
    // gx1 = x1^3 + A * x1^2 + x1
    let mut gx1 = curve(&x1);
    // x2 = -x1 - A
    let mut x2 = carry(&sub(&carry(&sub(&ZERO, &x1)), &A));
    let mut x = select(&x2, &x1, is_square(&gx1));
    let out = x.to_bytes();
    u.zeroize();
    u2.zeroize();
    tv1.zeroize();
    x1.zeroize();
    gx1.zeroize();
    x2.zeroize();
    x.zeroize();
    out
}

/// RFC 7748 §5's Montgomery ladder over the clamped scalar `n`, bit 254 down to bit 0, with the
/// RFC's names: `(x_2 : z_2)` and `(x_3 : z_3)` are the two points, swapped by [`cswap`] when a
/// scalar bit differs from the one before it. Every value the ladder computes, the RFC's named
/// ones and each sum, difference and product inside its `x_3`, `z_3` and `z_2` lines, has a
/// named local, so the last iteration's values are all wiped.
fn ladder(n: &[u8; 32], p: &[u8; 32]) -> [u8; 32] {
    let mut k: [u8; 32] = *n;
    k[0] &= 248;
    k[31] = (k[31] & 127) | 64;
    let mut x_1 = Fe::from_bytes(p);
    let (mut x_2, mut z_2, mut x_3, mut z_3) = (ONE, ZERO, x_1, ONE);
    let [
        mut a,
        mut b,
        mut c,
        mut d,
        mut e,
        mut da_plus_cb,
        mut da_minus_cb,
        mut aa_plus_a24_e,
    ] = [Loose([0; 5]); 8];
    let [
        mut aa,
        mut bb,
        mut da,
        mut cb,
        mut da_minus_cb_squared,
        mut a24_e,
    ] = [ZERO; 6];
    let mut swap = 0u64;
    for t in (0..=254usize).rev() {
        let k_t = u64::from((k[t >> 3] >> (t & 7)) & 1);
        swap ^= k_t;
        cswap(&mut x_2, &mut x_3, swap);
        cswap(&mut z_2, &mut z_3, swap);
        swap = k_t;
        a = add(&x_2, &z_2);
        aa = sqr(&a);
        b = sub(&x_2, &z_2);
        bb = sqr(&b);
        e = sub(&aa, &bb);
        c = add(&x_3, &z_3);
        d = sub(&x_3, &z_3);
        da = mul(&d, &a);
        cb = mul(&c, &b);
        da_plus_cb = add(&da, &cb);
        x_3 = sqr(&da_plus_cb);
        da_minus_cb = sub(&da, &cb);
        da_minus_cb_squared = sqr(&da_minus_cb);
        z_3 = mul(&x_1, &da_minus_cb_squared);
        x_2 = mul(&aa, &bb);
        a24_e = mul_a24(&e);
        aa_plus_a24_e = add(&aa, &a24_e);
        z_2 = mul(&e, &aa_plus_a24_e);
    }
    cswap(&mut x_2, &mut x_3, swap);
    cswap(&mut z_2, &mut z_3, swap);
    let mut inverse = invert(&z_2);
    let q = mul(&x_2, &inverse).to_bytes();
    k.zeroize();
    x_1.zeroize();
    x_2.zeroize();
    z_2.zeroize();
    x_3.zeroize();
    z_3.zeroize();
    for loose in [
        &mut a,
        &mut b,
        &mut c,
        &mut d,
        &mut e,
        &mut da_plus_cb,
        &mut da_minus_cb,
        &mut aa_plus_a24_e,
    ] {
        loose.zeroize();
    }
    for product in [
        &mut aa,
        &mut bb,
        &mut da,
        &mut cb,
        &mut da_minus_cb_squared,
        &mut a24_e,
        &mut inverse,
    ] {
        product.zeroize();
    }
    swap.zeroize();
    q
}

/// The field and ladder this one replaced, kept as the tests' oracle.
#[cfg(test)]
mod tweetnacl;

#[cfg(test)]
#[path = "x25519_tests.rs"]
mod tests;
