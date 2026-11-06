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
mod tests {
    use super::*;

    fn hex32(text: &str) -> [u8; 32] {
        let mut bytes = [0u8; 32];
        assert!(
            aterm_codec::hex::decode_into(text.as_bytes(), &mut bytes),
            "hex"
        );
        bytes
    }

    #[test]
    fn rfc7748_5_2_single_evaluations() {
        for (scalar, u, out) in [
            (
                "a546e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449ac4",
                "e6db6867583030db3594c1a424b15f7c726624ec26b3353b10a903a6d0ab1c4c",
                "c3da55379de9c6908e94ea4df28d084f32eccf03491c71f754b4075577a28552",
            ),
            (
                "4b66e9d4d1b4673c5ad22691957d6af5c11b6421e0ea01d42ca4169e7918ba0d",
                "e5210f12786811d3f4b7959d0538ae2c31dbe7106fc03c3efc4cd549c715a493",
                "95cbde9476e8907d7aade45cb4b873f88b595a68799fa152e6f8f7647aac7957",
            ),
        ] {
            assert_eq!(x25519(&hex32(scalar), &hex32(u)), hex32(out));
        }
    }

    fn iterate(times: usize) -> [u8; 32] {
        let mut k = BASEPOINT;
        let mut u = BASEPOINT;
        for _ in 0..times {
            let next = x25519(&k, &u);
            u = k;
            k = next;
        }
        k
    }

    #[test]
    fn rfc7748_5_2_iterated_once_and_a_thousand_times() {
        assert_eq!(
            iterate(1),
            hex32("422c8e7a6227d7bca1350b3e2bb7279f7897b87bb6854b783c60e80311ae3079")
        );
        assert_eq!(
            iterate(1000),
            hex32("684cf59ba83309552800ef566f2f4d3c1c3887c49360e3875f2eb94d99532c51")
        );
    }

    #[test]
    #[ignore = "RFC 7748's million-iteration vector takes about half a minute (27 s in the test profile); run with --ignored"]
    fn rfc7748_5_2_iterated_a_million_times() {
        assert_eq!(
            iterate(1_000_000),
            hex32("7c3911e0ab2586fd864497297e575e6f3bc601c0883c30df5f4dd2d24f665424")
        );
    }

    #[test]
    fn rfc7748_6_1_diffie_hellman() {
        let alice = hex32("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
        let alice_public =
            hex32("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a");
        let bob = hex32("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb");
        let bob_public = hex32("de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f");
        let shared = hex32("4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742");
        assert_eq!(x25519_base(&alice), alice_public);
        assert_eq!(x25519_base(&bob), bob_public);
        assert_eq!(x25519(&alice, &bob_public), shared);
        assert_eq!(x25519(&bob, &alice_public), shared);
    }

    #[test]
    fn low_order_inputs_give_all_zero_output() {
        let mut points = vec![
            "0000000000000000000000000000000000000000000000000000000000000000",
            "0100000000000000000000000000000000000000000000000000000000000000",
            "e0eb7a7c3b41b8ae1656e3faf19fc46ada098deb9c32b1fd866205165f49b800",
            "5f9c95bca3508c24b1d0b1559c83ef5b04445cc4581c8e86d8224eddd09f1157",
            "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
            "edffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
            "eeffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
        ]
        .into_iter()
        .map(hex32)
        .collect::<Vec<_>>();
        let top_bit_set: Vec<[u8; 32]> = points
            .iter()
            .map(|point| {
                let mut point = *point;
                point[31] |= 0x80;
                point
            })
            .collect();
        points.extend(top_bit_set);
        for seed in [[7u8; 32], [0u8; 32], [0xffu8; 32]] {
            for point in &points {
                assert_eq!(x25519(&seed, point), [0u8; 32]);
            }
        }
    }

    /// RFC 9380 prints field elements big-endian (I2OSP); X25519 reads them little-endian.
    fn be32(text: &str) -> [u8; 32] {
        let mut bytes = hex32(text);
        bytes.reverse();
        bytes
    }

    /// RFC 9380 Appendix J.4's curve25519 vectors, as (`u`, `Q.x`, `Q.y`) for each map call:
    /// J.4.1 (`curve25519_XMD:SHA-512_ELL2_RO_`, `Q0` and `Q1` per message) and J.4.2
    /// (`curve25519_XMD:SHA-512_ELL2_NU_`, `Q` per message), copied byte for byte.
    const RFC9380_J4: [(&str, &str, &str); 15] = [
        (
            "005fe8a7b8fef0a16c105e6cadf5a6740b3365e18692a9c05bfbb4d97f645a6a",
            "36b4df0c864c64707cbf6cf36e9ee2c09a6cb93b28313c169be29561bb904f98",
            "6cd59d664fb58c66c892883cd0eb792e52055284dac3907dd756b45d15c3983d",
        ),
        (
            "1347edbec6a2b5d8c02e058819819bee177077c9d10a4ce165aab0fd0252261a",
            "3fa114783a505c0b2b2fbeef0102853c0b494e7757f2a089d0daae7ed9a0db2b",
            "76c0fe7fec932aaafb8eefb42d9cbb32eb931158f469ff3050af15cfdbbeff94",
        ),
        (
            "49bed021c7a3748f09fa8cdfcac044089f7829d3531066ac9e74e0994e05bc7d",
            "16b3d86e056b7970fa00165f6f48d90b619ad618791661b7b5e1ec78be10eac1",
            "4ab256422d84c5120b278cbdfc4e1facc5baadffeccecf8ee9bf3946106d50ca",
        ),
        (
            "5c36525b663e63389d886105cee7ed712325d5a97e60e140aba7e2ce5ae851b6",
            "7ec29ddbf34539c40adfa98fcb39ec36368f47f30e8f888cc7e86f4d46e0c264",
            "10d1abc1cae2d34c06e247f2141ba897657fb39f1080d54f09ce0af128067c74",
        ),
        (
            "6412b7485ba26d3d1b6c290a8e1435b2959f03721874939b21782df17323d160",
            "71de3dadfe268872326c35ac512164850860567aea0e7325e6b91a98f86533ad",
            "26a08b6e9a18084c56f2147bf515414b9b63f1522e1b6c5649f7d4b0324296ec",
        ),
        (
            "24c7b46c1c6d9a21d32f5707be1380ab82db1054fde82865d5c9e3d968f287b2",
            "5704069021f61e41779e2ba6b932268316d6d2a6f064f997a22fef16d1eaeaca",
            "50483c7540f64fb4497619c050f2c7fe55454ec0f0e79870bb44302e34232210",
        ),
        (
            "5e123990f11bbb5586613ffabdb58d47f64bb5f2fa115f8ea8df0188e0c9e1b5",
            "7a94d45a198fb5daa381f45f2619ab279744efdd8bd8ed587fc5b65d6cea1df0",
            "67d44f85d376e64bb7d713585230cdbfafc8e2676f7568e0b6ee59361116a6e1",
        ),
        (
            "5e8553eb00438a0bb1e7faa59dec6d8087f9c8011e5fb8ed9df31cb6c0d4ac19",
            "30506fb7a32136694abd61b6113770270debe593027a968a01f271e146e60c18",
            "7eeee0e706b40c6b5174e551426a67f975ad5a977ee2f01e8e20a6d612458c3b",
        ),
        (
            "20f481e85da7a3bf60ac0fb11ed1d0558fc6f941b3ac5469aa8b56ec883d6d7d",
            "02d606e2699b918ee36f2818f2bc5013e437e673c9f9b9cdc15fd0c5ee913970",
            "29e9dc92297231ef211245db9e31767996c5625dfbf92e1c8107ef887365de1e",
        ),
        (
            "017d57fd257e9a78913999a23b52ca988157a81b09c5442501d07fed20869465",
            "38920e9b988d1ab7449c0fa9a6058192c0c797bb3d42ac345724341a1aa98745",
            "24dcc1be7c4d591d307e89049fd2ed30aae8911245a9d8554bf6032e5aa40d3d",
        ),
        (
            "608d892b641f0328523802a6603427c26e55e6f27e71a91a478148d45b5093cd",
            "51125222da5e763d97f3c10fcc92ea6860b9ccbbd2eb1285728f566721c1e65b",
            "343d2204f812d3dfc5304a5808c6c0d81a903a5d228b342442aa3c9ba5520a3d",
        ),
        (
            "46f5b22494bfeaa7f232cc8d054be68561af50230234d7d1d63d1d9abeca8da5",
            "7d56d1e08cb0ccb92baf069c18c49bb5a0dcd927eff8dcf75ca921ef7f3e6eeb",
            "404d9a7dc25c9c05c44ab9a94590e7c3fe2dcec74533a0b24b188a5d5dacf429",
        ),
        (
            "235fe40c443766ce7e18111c33862d66c3b33267efa50d50f9e8e5d252a40aaa",
            "3fbe66b9c9883d79e8407150e7c2a1c8680bee496c62fabe4619a72b3cabe90f",
            "08ec476147c9a0a3ff312d303dbbd076abb7551e5fce82b48ab14b433f8d0a7b",
        ),
        (
            "001e92a544463bda9bd04ddbe3d6eed248f82de32f522669efc5ddce95f46f5b",
            "227e0bb89de700385d19ec40e857db6e6a3e634b1c32962f370d26f84ff19683",
            "5f86ff3851d262727326a32c1bf7655a03665830fa7f1b8b1e5a09d85bc66e4a",
        ),
        (
            "1a68a1af9f663592291af987203393f707305c7bac9c8d63d6a729bdc553dc19",
            "3bcd651ee54d5f7b6013898aab251ee8ecc0688166fce6e9548d38472f6bd196",
            "1bb36ad9197299f111b4ef21271c41f4b7ecf5543db8bb5931307ebdb2eaa465",
        ),
    ];

    #[test]
    fn rfc9380_curve25519_elligator2_vectors() {
        for (u, qx, qy) in RFC9380_J4 {
            let u = be32(u);
            assert_eq!(elligator2(&u), be32(qx), "Q.x for u = {u:02x?}");
            // RFC 9380 §6.7.1 steps 6-7: `y` is odd exactly when `x1` was taken, that is, when
            // `is_square(gx1)` held, so the vector's `Q.y` says what `is_square` must answer.
            let field = Fe::from_bytes(&u);
            let tv1 = sqr(&field);
            let x1 = mul(
                &sub(&ZERO, &A),
                &invert(&add(&carry(&add(&tv1, &tv1)), &ONE)),
            );
            let odd = u64::from(be32(qy)[0] & 1);
            assert_eq!(
                is_square(&curve(&x1)),
                odd,
                "is_square(gx1) for u = {u:02x?}"
            );
            // Q lies on the curve: y^2 = gx(Q.x), a square.
            let y = Fe::from_bytes(&be32(qy));
            let gx = curve(&Fe::from_bytes(&be32(qx)));
            assert_eq!(equal(&sqr(&y), &gx), 1);
            assert_eq!(is_square(&gx), 1);
        }
    }

    #[test]
    fn is_square_answers_for_squares_non_squares_and_zero() {
        // RFC 9380 picks Z = 2 for curve25519 because 2 is no square; -1 is one, as p = 1 mod 4.
        let two = Fe([2, 0, 0, 0, 0]);
        assert_eq!(is_square(&two), 0);
        assert_eq!(is_square(&ZERO), 1);
        assert_eq!(is_square(&ONE), 1);
        assert_eq!(is_square(&carry(&sub(&ZERO, &ONE))), 1);
        for _ in 0..32 {
            let mut bytes = [0u8; 32];
            aterm_uds::rand::fill(&mut bytes).expect("CSPRNG");
            let r = Fe::from_bytes(&bytes);
            if equal(&r, &ZERO) == 1 {
                continue;
            }
            assert_eq!(is_square(&r) == 1, has_root(&r), "Euler and a root agree");
            let square = sqr(&r);
            assert_eq!(is_square(&square), 1);
            assert_eq!(is_square(&mul(&square, &two)), 0);
        }
    }

    #[test]
    fn select_and_equal_choose_by_value_not_by_limbs() {
        let five = Fe([5, 0, 0, 0, 0]);
        assert_eq!(select(&ONE, &five, 0).0, ONE.0);
        assert_eq!(select(&ONE, &five, 1).0, five.0);
        // p + 5 in limbs (unreduced) is the element 5.
        let p_plus_five = Fe::from_bytes(&hex32(
            "f2ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
        ));
        assert_ne!(p_plus_five.0, five.0);
        assert_eq!(equal(&p_plus_five, &five), 1);
        assert_eq!(equal(&five, &ONE), 0);
        // A difference in the last byte only.
        let mut top = [0u8; 32];
        top[31] = 0x40;
        assert_eq!(equal(&Fe::from_bytes(&top), &ZERO), 0);
    }

    /// Whether `x` is a square, by a route that shares no code with [`legendre`] or [`equal`]:
    /// `c = x^((p+3)/8)` squares to `x` or `-x` exactly when `x` is a square, as p = 5 mod 8.
    fn has_root(x: &Fe) -> bool {
        // (p+3)/8 = 2^252 - 2: every bit from 251 down set but bit 0.
        let mut c = *x;
        for a in (0..=250).rev() {
            c = sqr(&c);
            if a != 0 {
                c = mul(&c, x);
            }
        }
        let c2 = sqr(&c).to_bytes();
        c2 == x.to_bytes() || c2 == carry(&sub(&ZERO, x)).to_bytes()
    }

    #[test]
    fn elligator2_lands_on_the_curve_and_ignores_bit_255() {
        for _ in 0..32 {
            let mut r = [0u8; 32];
            aterm_uds::rand::fill(&mut r).expect("CSPRNG");
            let x = elligator2(&r);
            assert_eq!(x[31] & 0x80, 0, "a canonical u-coordinate");
            assert!(canonical_u(&x));
            assert!(
                has_root(&curve(&Fe::from_bytes(&x))),
                "on the curve, not its twist"
            );
            let mut flipped = r;
            flipped[31] ^= 0x80;
            assert_eq!(elligator2(&flipped), x);
        }
    }

    #[test]
    fn non_canonical_u_is_reduced() {
        let scalar = [7u8; 32];
        let nine_plus_p = hex32("f6ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f");
        assert_eq!(x25519(&scalar, &nine_plus_p), x25519_base(&scalar));
    }

    #[test]
    fn agrees_with_ring_on_random_keys() {
        use ring::agreement::{EphemeralPrivateKey, UnparsedPublicKey, X25519, agree_ephemeral};
        use ring::rand::SystemRandom;

        let rng = SystemRandom::new();
        for _ in 0..64 {
            let theirs = EphemeralPrivateKey::generate(&X25519, &rng).expect("ring key");
            let their_public = theirs.compute_public_key().expect("ring public key");
            let again = theirs.compute_public_key().expect("ring public key");
            assert_eq!(
                their_public.as_ref(),
                again.as_ref(),
                "ring's public key is stable"
            );
            let their_public: [u8; 32] = their_public.as_ref().try_into().expect("32 bytes");

            let mut ours = [0u8; 32];
            aterm_uds::rand::fill(&mut ours).expect("CSPRNG");
            let our_public = x25519_base(&ours);
            assert_eq!(our_public[31] & 0x80, 0, "a public key is canonical");

            let ring_shared =
                agree_ephemeral(theirs, &UnparsedPublicKey::new(&X25519, our_public), |k| {
                    <[u8; 32]>::try_from(k).expect("32 bytes")
                })
                .expect("ring agrees");
            let our_shared = x25519(&ours, &their_public);
            assert_eq!(our_shared, ring_shared);
            assert_ne!(our_shared, [0u8; 32]);
        }
    }

    /// SplitMix64 (Steele, Lea and Flood, OOPSLA 2014): the differential tests' inputs, the same
    /// on every run, so a disagreement is found again by the same seed.
    struct SplitMix(u64);

    impl SplitMix {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        }

        fn bytes(&mut self) -> [u8; 32] {
            let mut out = [0u8; 32];
            for chunk in out.as_chunks_mut::<8>().0 {
                *chunk = self.next().to_le_bytes();
            }
            out
        }

        /// A limb in `0..=most`, the ends included one time in eight each.
        fn limb(&mut self, most: u64) -> u64 {
            match self.next() % 8 {
                0 => 0,
                1 => most,
                _ => self.next() % (most + 1),
            }
        }
    }

    /// Every encoding the ladder and the map must treat as RFC 7748 does at an edge: the 14
    /// low-order spellings, 0 to 31, every value from p - 32 to 2^255 - 1 (unreduced from p on),
    /// and each of those with bit 255 set.
    fn edge_encodings() -> Vec<[u8; 32]> {
        let mut points: Vec<[u8; 32]> = [
            "e0eb7a7c3b41b8ae1656e3faf19fc46ada098deb9c32b1fd866205165f49b800",
            "5f9c95bca3508c24b1d0b1559c83ef5b04445cc4581c8e86d8224eddd09f1157",
        ]
        .into_iter()
        .map(hex32)
        .collect();
        for small in 0..32u8 {
            let mut point = [0u8; 32];
            point[0] = small;
            points.push(point);
        }
        // 2^255 - 1 - k for k = 0..=50: p - 32 up to 2^255 - 1, with p - 1, p and p + 1 among them.
        for k in 0..=50u8 {
            let mut point = [0xff; 32];
            point[0] = 0xff - k;
            point[31] = 0x7f;
            points.push(point);
        }
        let top_bit_set: Vec<[u8; 32]> = points
            .iter()
            .map(|point| {
                let mut point = *point;
                point[31] |= 0x80;
                point
            })
            .collect();
        points.extend(top_bit_set);
        points
    }

    /// The oracle's element with the value of radix-2^51 limbs (each below 2^53), built from
    /// 16-bit pieces and the powers 2^(51 i), so that no limb code of the new field takes part.
    fn oracle_of(limbs: [u64; 5]) -> tweetnacl::Gf {
        let mut sum = tweetnacl::ZERO;
        for (i, limb) in limbs.into_iter().enumerate() {
            let mut power = [0u8; 32];
            power[51 * i / 8] = 1 << (51 * i % 8);
            let mut pieces = tweetnacl::ZERO;
            for (j, piece) in pieces.iter_mut().take(4).enumerate() {
                *piece = i64::try_from((limb >> (16 * j)) & 0xffff).expect("16 bits");
            }
            sum = tweetnacl::add(
                &sum,
                &tweetnacl::mul(&pieces, &tweetnacl::unpack25519(&power)),
            );
        }
        tweetnacl::mul(&sum, &tweetnacl::ONE)
    }

    /// The radix-2^51 field against TweetNaCl's, by value: decoding and encoding, products,
    /// squares, the a24 product, sums and differences, inversion and Euler's criterion, over
    /// fixed-seed limbs across the whole of each operation's bounds (both ends included, so a
    /// limb or column that overflowed would panic under the tests' overflow checks) and over the
    /// edge encodings. Negative controls: dropping `carry_columns`' last carry fails here, as it
    /// fails RFC 7748's iterated and Diffie-Hellman vectors; `A24` = 121666 fails the a24 product.
    #[test]
    fn the_radix_2_51_field_agrees_with_tweetnacls() {
        use tweetnacl as old;
        let mut seed = SplitMix(0x7748_9380_2551_9000);
        let tight = |seed: &mut SplitMix| Fe([(); 5].map(|()| seed.limb(1 << 51)));
        let loose = |seed: &mut SplitMix| Loose([(); 5].map(|()| seed.limb(3 << 51)));
        for encoding in edge_encodings() {
            assert_eq!(
                Fe::from_bytes(&encoding).to_bytes(),
                old::pack25519(&old::unpack25519(&encoding)),
                "{encoding:02x?}"
            );
        }
        let extremes = [Fe([1 << 51; 5]), Fe(P), Fe([LOW_51; 5]), ZERO];
        for f in extremes {
            assert_eq!(f.to_bytes(), old::pack25519(&oracle_of(f.0)), "{:x?}", f.0);
        }
        let a24 = old::unpack25519(&{
            let mut bytes = [0u8; 32];
            bytes[..4].copy_from_slice(&121_665u32.to_le_bytes());
            bytes
        });
        for round in 0..2048 {
            let (a, b) = (loose(&mut seed), loose(&mut seed));
            let (x, y) = (oracle_of(a.0), oracle_of(b.0));
            let mul_ok = mul(&a, &b).to_bytes() == old::pack25519(&old::mul(&x, &y));
            assert!(mul_ok, "mul {:x?} {:x?}", a.0, b.0);
            assert_eq!(
                sqr(&a).to_bytes(),
                old::pack25519(&old::sqr(&x)),
                "sqr {:x?}",
                a.0
            );
            assert_eq!(
                mul_a24(&a).to_bytes(),
                old::pack25519(&old::mul(&x, &a24)),
                "a24 {:x?}",
                a.0
            );
            assert_eq!(carry(&a).to_bytes(), old::pack25519(&x), "carry {:x?}", a.0);
            let (f, g) = (tight(&mut seed), tight(&mut seed));
            let (u, v) = (oracle_of(f.0), oracle_of(g.0));
            assert_eq!(f.to_bytes(), old::pack25519(&u), "to_bytes {:x?}", f.0);
            assert_eq!(
                carry(&add(&f, &g)).to_bytes(),
                old::pack25519(&old::add(&u, &v)),
                "add {:x?} {:x?}",
                f.0,
                g.0
            );
            assert_eq!(
                carry(&sub(&f, &g)).to_bytes(),
                old::pack25519(&old::mul(&old::sub(&u, &v), &old::ONE)),
                "sub {:x?} {:x?}",
                f.0,
                g.0
            );
            if round % 16 == 0 {
                assert_eq!(
                    invert(&a).to_bytes(),
                    old::pack25519(&old::inv25519(&x)),
                    "invert {:x?}",
                    a.0
                );
                assert_eq!(
                    legendre(&f).to_bytes(),
                    old::pack25519(&old::legendre(&u)),
                    "legendre {:x?}",
                    f.0
                );
                assert_eq!(is_square(&f), u64::from(old::is_square(&u) == 1));
            }
        }
    }

    /// The ladder against TweetNaCl's, output for output: 1024 fixed-seed scalars and points, and
    /// three scalars (all zeros, all ones, a seeded one) at every edge encoding (low order, small,
    /// near p, unreduced, bit 255 set). Negative control: `A24` = 121666 fails here, and so do
    /// RFC 7748's vectors.
    #[test]
    fn the_radix_2_51_ladder_agrees_with_tweetnacls() {
        let mut seed = SplitMix(0x7748_0005_0002_0001);
        for _ in 0..1024 {
            let (scalar, point) = (seed.bytes(), seed.bytes());
            assert_eq!(
                ladder(&scalar, &point),
                tweetnacl::ladder(&scalar, &point),
                "{scalar:02x?} {point:02x?}"
            );
        }
        let scalars = [[0u8; 32], [0xff; 32], seed.bytes()];
        for point in edge_encodings() {
            for scalar in &scalars {
                assert_eq!(
                    ladder(scalar, &point),
                    tweetnacl::ladder(scalar, &point),
                    "{scalar:02x?} {point:02x?}"
                );
            }
        }
    }

    /// The map against TweetNaCl's, output for output, over 512 fixed-seed inputs and every
    /// edge encoding. Negative control: `x2 = -x1 + A` fails here and RFC 9380's vectors.
    #[test]
    fn elligator2_agrees_with_tweetnacls() {
        let mut seed = SplitMix(0x9380_0006_0007_0001);
        let inputs: Vec<[u8; 32]> = (0..512).map(|_| seed.bytes()).collect();
        for r in inputs.into_iter().chain(edge_encodings()) {
            assert_eq!(elligator2(&r), tweetnacl::elligator2(&r), "{r:02x?}");
        }
    }
}
