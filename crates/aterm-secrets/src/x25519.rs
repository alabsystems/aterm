// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! X25519 (RFC 7748 §5) scalar multiplication (`crypto_scalarmult`), and the Elligator 2 map
//! onto curve25519 (RFC 9380 §6.7.1) that CPace's generator needs ([`elligator2`]).
//!
//! The field arithmetic, the ladder and the map run in constant time in their secret inputs: no
//! branch, loop bound or memory index depends on a field element or a scalar bit. Loops run over
//! fixed limb counts and public exponents, and choices are made by masks ([`sel25519`],
//! [`select`], [`equal`]), each behind `black_box`, so the optimizer cannot learn that a mask is
//! all ones or zero and turn it back into a branch. [`canonical_u`], which checks a public key,
//! is the one function that compares by value.

use zeroize::Zeroize;

/// The u-coordinate 9, the base point.
pub const BASEPOINT: [u8; 32] = {
    let mut point = [0u8; 32];
    point[0] = 9;
    point
};

type Gf = [i64; 16];

const A24: Gf = [0xDB41, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

/// curve25519's Montgomery coefficient `A` = 486662 = 0x76d06 (RFC 7748 §4.1).
const A: Gf = [0x6D06, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

const ZERO: Gf = [0; 16];

const ONE: Gf = [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

/// X25519(scalar, u): the u-coordinate of the clamped scalar times the point with u-coordinate `u`.
#[inline]
#[must_use]
pub fn x25519(scalar: &[u8; 32], u: &[u8; 32]) -> [u8; 32] {
    crypto_scalarmult(scalar, u)
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

#[inline]
fn car25519(o: &mut Gf) {
    for i in 0..16 {
        let fold: i64 = if i < 15 { 1 } else { 38 };
        o[(i + 1) % 16] += fold * (o[i] >> 16);
        o[i] &= 0xffff;
    }
}

#[inline]
fn sel25519(p: &mut Gf, q: &mut Gf, b: i64) {
    let c = std::hint::black_box(!(b - 1));
    for i in 0..16 {
        let t = c & (p[i] ^ q[i]);
        p[i] ^= t;
        q[i] ^= t;
    }
}

fn pack25519(n: &Gf) -> [u8; 32] {
    let mut t: Gf = *n;
    let mut m: Gf = [0; 16];
    car25519(&mut t);
    car25519(&mut t);
    car25519(&mut t);
    for _ in 0..2 {
        m[0] = t[0] - 0xffed;
        for i in 1..15 {
            m[i] = t[i] - 0xffff - ((m[i - 1] >> 16) & 1);
            m[i - 1] &= 0xffff;
        }
        m[15] = t[15] - 0x7fff - ((m[14] >> 16) & 1);
        let b = (m[15] >> 16) & 1;
        m[14] &= 0xffff;
        sel25519(&mut t, &mut m, 1 - b);
    }
    let mut o = [0u8; 32];
    for i in 0..16 {
        o[2 * i] = (t[i] & 0xff) as u8;
        o[2 * i + 1] = ((t[i] >> 8) & 0xff) as u8;
    }
    t.zeroize();
    m.zeroize();
    o
}

fn unpack25519(n: &[u8; 32]) -> Gf {
    let mut o: Gf = [0; 16];
    for i in 0..16 {
        o[i] = i64::from(n[2 * i]) + (i64::from(n[2 * i + 1]) << 8);
    }
    o[15] &= 0x7fff;
    o
}

#[inline]
fn add(a: &Gf, b: &Gf) -> Gf {
    let mut o: Gf = [0; 16];
    for i in 0..16 {
        o[i] = a[i] + b[i];
    }
    o
}

#[inline]
fn sub(a: &Gf, b: &Gf) -> Gf {
    let mut o: Gf = [0; 16];
    for i in 0..16 {
        o[i] = a[i] - b[i];
    }
    o
}

#[inline]
fn mul(a: &Gf, b: &Gf) -> Gf {
    let mut t = [0i64; 31];
    for i in 0..16 {
        for j in 0..16 {
            t[i + j] += a[i] * b[j];
        }
    }
    for i in 0..15 {
        t[i] += 38 * t[i + 16];
    }
    let mut o: Gf = [0; 16];
    o.copy_from_slice(&t[..16]);
    car25519(&mut o);
    car25519(&mut o);
    o
}

#[inline]
fn sqr(a: &Gf) -> Gf {
    mul(a, a)
}

fn inv25519(i: &Gf) -> Gf {
    let mut c: Gf = *i;
    for a in (0..=253).rev() {
        c = sqr(&c);
        if a != 2 && a != 4 {
            c = mul(&c, i);
        }
    }
    c
}

/// `a` when `bit` is 0 and `b` when it is 1, chosen by a mask, never a branch. `bit` must be 0
/// or 1.
#[inline]
fn select(a: &Gf, b: &Gf, bit: i64) -> Gf {
    let mask = std::hint::black_box(-bit);
    let mut o: Gf = *a;
    for i in 0..16 {
        o[i] ^= mask & (a[i] ^ b[i]);
    }
    o
}

/// 1 when `a` and `b` are the same element of GF(p), whatever their limbs, else 0. Both are
/// packed to their canonical bytes and every byte pair is folded with no early exit.
fn equal(a: &Gf, b: &Gf) -> i64 {
    let mut x = pack25519(a);
    let mut y = pack25519(b);
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= x[i] ^ y[i];
    }
    x.zeroize();
    y.zeroize();
    // `diff - 1` is negative, so all ones after the arithmetic shift, exactly when `diff` is 0.
    std::hint::black_box(((i64::from(std::hint::black_box(diff)) - 1) >> 8) & 1)
}

/// `i^((p-1)/2)`, Euler's criterion: 1 for a nonzero square, -1 for a non-square, 0 for 0.
/// (p-1)/2 = 2^254 - 10 has every bit from 253 down set but bits 3 and 0, and that public
/// exponent alone shapes the loop, as p-2 shapes [`inv25519`].
fn legendre(i: &Gf) -> Gf {
    let mut c: Gf = *i;
    for a in (0..=252).rev() {
        c = sqr(&c);
        if a != 3 && a != 0 {
            c = mul(&c, i);
        }
    }
    c
}

/// RFC 9380 §4's `is_square`: 1 when `i` is a square in GF(p), zero included, else 0.
fn is_square(i: &Gf) -> i64 {
    let mut l = legendre(i);
    let square = equal(&l, &ONE) | equal(&l, &ZERO);
    l.zeroize();
    square
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
    let mut u = unpack25519(r);
    let mut tv1 = sqr(&u);
    tv1 = add(&tv1, &tv1);
    // x1 = -A / (1 + Z * u^2)
    let mut x1 = mul(&sub(&ZERO, &A), &inv25519(&add(&tv1, &ONE)));
    // gx1 = x1^3 + A * x1^2 + x1
    let mut gx1 = mul(&add(&mul(&add(&x1, &A), &x1), &ONE), &x1);
    // x2 = -x1 - A, normalized through `mul` as the ladder normalizes a `sub` before packing.
    let mut x2 = mul(&sub(&sub(&ZERO, &x1), &A), &ONE);
    let mut x = select(&x2, &x1, is_square(&gx1));
    let out = pack25519(&x);
    u.zeroize();
    tv1.zeroize();
    x1.zeroize();
    gx1.zeroize();
    x2.zeroize();
    x.zeroize();
    out
}

fn crypto_scalarmult(n: &[u8; 32], p: &[u8; 32]) -> [u8; 32] {
    #[cfg(any(test, debug_assertions))]
    if seam::memoized() {
        return seam::through(n, p, ladder);
    }
    ladder(n, p)
}

fn ladder(n: &[u8; 32], p: &[u8; 32]) -> [u8; 32] {
    let mut z: [u8; 32] = *n;
    z[31] = (n[31] & 127) | 64;
    z[0] &= 248;
    let mut x = unpack25519(p);
    let mut a: Gf = [0; 16];
    let mut b: Gf = x;
    let mut c: Gf = [0; 16];
    let mut d: Gf = [0; 16];
    let mut e: Gf;
    let mut f: Gf;
    a[0] = 1;
    d[0] = 1;
    for i in (0..=254usize).rev() {
        let r = i64::from((z[i >> 3] >> (i & 7)) & 1);
        sel25519(&mut a, &mut b, r);
        sel25519(&mut c, &mut d, r);
        e = add(&a, &c);
        a = sub(&a, &c);
        c = add(&b, &d);
        b = sub(&b, &d);
        d = sqr(&e);
        f = sqr(&a);
        a = mul(&c, &a);
        c = mul(&b, &e);
        e = add(&a, &c);
        a = sub(&a, &c);
        b = sqr(&a);
        c = sub(&d, &f);
        a = mul(&c, &A24);
        a = add(&a, &d);
        c = mul(&c, &a);
        a = mul(&d, &f);
        d = mul(&b, &x);
        b = sqr(&e);
        sel25519(&mut a, &mut b, r);
        sel25519(&mut c, &mut d, r);
        e.zeroize();
        f.zeroize();
    }
    c = inv25519(&c);
    a = mul(&a, &c);
    let q = pack25519(&a);
    z.zeroize();
    x.zeroize();
    a.zeroize();
    b.zeroize();
    c.zeroize();
    d.zeroize();
    q
}

/// A test binary's memo of X25519 ([`crate::memoized_x25519_in_tests!`]). The function is
/// pure, so a test that reads one vault at every lock it takes (each read unwraps the file key
/// with the device's scalar) computes each scalar and point once, and the answer is the one the
/// ladder gives. Test and debug builds only, and only once a test binary asks: the memo keeps
/// scalars in memory and answers in variable time, which no shipped process may do.
#[cfg(any(test, debug_assertions))]
pub mod seam {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, PoisonError};

    /// The most pairs the memo keeps before it starts over.
    const MOST: usize = 1 << 16;

    /// Each scalar and point computed, with the ladder's answer.
    type Memo = BTreeMap<([u8; 32], [u8; 32]), [u8; 32]>;

    static ASKED: AtomicBool = AtomicBool::new(false);
    static MEMO: Mutex<Memo> = Mutex::new(BTreeMap::new());

    /// Memoize this process's X25519 from now on: a test binary's load-time hook calls it.
    pub fn memoize() {
        ASKED.store(true, Ordering::Relaxed);
    }

    /// Whether this process memoizes X25519: aterm-secrets' own tests, and a test binary that
    /// asked.
    pub(super) fn memoized() -> bool {
        cfg!(test) || ASKED.load(Ordering::Relaxed)
    }

    /// `compute(scalar, point)`, from the memo when this pair was computed before.
    pub(super) fn through(
        scalar: &[u8; 32],
        point: &[u8; 32],
        compute: fn(&[u8; 32], &[u8; 32]) -> [u8; 32],
    ) -> [u8; 32] {
        let key = (*scalar, *point);
        let known = MEMO
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
            .copied();
        if let Some(out) = known {
            return out;
        }
        let out = compute(scalar, point);
        let mut memo = MEMO.lock().unwrap_or_else(PoisonError::into_inner);
        if memo.len() >= MOST {
            memo.clear();
        }
        memo.insert(key, out);
        out
    }
}

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
    #[ignore = "RFC 7748's million-iteration vector takes minutes even optimized (203 s under --profile opt-test) and hours unoptimized; run with --ignored"]
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

    /// `x^3 + A x^2 + x`, the right side of curve25519's equation.
    fn curve(x: &Gf) -> Gf {
        mul(&add(&mul(&add(x, &A), x), &ONE), x)
    }

    #[test]
    fn rfc9380_curve25519_elligator2_vectors() {
        for (u, qx, qy) in RFC9380_J4 {
            let u = be32(u);
            assert_eq!(elligator2(&u), be32(qx), "Q.x for u = {u:02x?}");
            // RFC 9380 §6.7.1 steps 6-7: `y` is odd exactly when `x1` was taken, that is, when
            // `is_square(gx1)` held, so the vector's `Q.y` says what `is_square` must answer.
            let field = unpack25519(&u);
            let tv1 = mul(&field, &field);
            let x1 = mul(&sub(&ZERO, &A), &inv25519(&add(&add(&tv1, &tv1), &ONE)));
            let odd = i64::from(be32(qy)[0] & 1);
            assert_eq!(
                is_square(&curve(&x1)),
                odd,
                "is_square(gx1) for u = {u:02x?}"
            );
            // Q lies on the curve: y^2 = gx(Q.x), a square.
            let y = unpack25519(&be32(qy));
            let gx = curve(&unpack25519(&be32(qx)));
            assert_eq!(equal(&mul(&y, &y), &gx), 1);
            assert_eq!(is_square(&gx), 1);
        }
    }

    #[test]
    fn is_square_answers_for_squares_non_squares_and_zero() {
        // RFC 9380 picks Z = 2 for curve25519 because 2 is no square; -1 is one, as p = 1 mod 4.
        let two: Gf = [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(is_square(&two), 0);
        assert_eq!(is_square(&ZERO), 1);
        assert_eq!(is_square(&ONE), 1);
        assert_eq!(is_square(&sub(&ZERO, &ONE)), 1);
        for _ in 0..32 {
            let mut bytes = [0u8; 32];
            aterm_uds::rand::fill(&mut bytes).expect("CSPRNG");
            let r = unpack25519(&bytes);
            if equal(&r, &ZERO) == 1 {
                continue;
            }
            assert_eq!(is_square(&r) == 1, has_root(&r), "Euler and a root agree");
            let square = mul(&r, &r);
            assert_eq!(is_square(&square), 1);
            assert_eq!(is_square(&mul(&square, &two)), 0);
        }
    }

    #[test]
    fn select_and_equal_choose_by_value_not_by_limbs() {
        let five: Gf = [5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(select(&ONE, &five, 0), ONE);
        assert_eq!(select(&ONE, &five, 1), five);
        // p + 5 in limbs (unreduced) is the element 5.
        let p_plus_five = unpack25519(&hex32(
            "f2ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
        ));
        assert_eq!(equal(&p_plus_five, &five), 1);
        assert_eq!(equal(&five, &ONE), 0);
        // A difference in the last byte only.
        let mut top = [0u8; 32];
        top[31] = 0x40;
        assert_eq!(equal(&unpack25519(&top), &ZERO), 0);
    }

    /// Whether `x` is a square, by a route that shares no code with [`legendre`] or [`equal`]:
    /// `c = x^((p+3)/8)` squares to `x` or `-x` exactly when `x` is a square, as p = 5 mod 8.
    fn has_root(x: &Gf) -> bool {
        // (p+3)/8 = 2^252 - 2: every bit from 251 down set but bit 0.
        let mut c = *x;
        for a in (0..=250).rev() {
            c = sqr(&c);
            if a != 0 {
                c = mul(&c, x);
            }
        }
        let c2 = pack25519(&sqr(&c));
        c2 == pack25519(x) || c2 == pack25519(&sub(&ZERO, x))
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
                has_root(&curve(&unpack25519(&x))),
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

    /// The memo computes a pair once and answers it again with the ladder's answer; another
    /// point (or scalar) of the pair is computed afresh. Negative control: a memo keyed by
    /// the scalar alone answered `x25519(alice, bob_public)` with Alice's public key, which
    /// `rfc7748_6_1_diffie_hellman` refuses.
    #[test]
    fn the_memo_answers_a_pair_with_what_the_ladder_computed() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COMPUTED: AtomicUsize = AtomicUsize::new(0);
        fn counted(n: &[u8; 32], p: &[u8; 32]) -> [u8; 32] {
            COMPUTED.fetch_add(1, Ordering::Relaxed);
            ladder(n, p)
        }
        assert!(seam::memoized(), "aterm-secrets' own tests memoize");
        let scalar = hex32("a546e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449ac4");
        let point = hex32("e6db6867583030db3594c1a424b15f7c726624ec26b3353b10a903a6d0ab1c4c");
        let out = hex32("c3da55379de9c6908e94ea4df28d084f32eccf03491c71f754b4075577a28552");
        let mut other = point;
        other[0] ^= 1;
        let mut scalar_only = scalar;
        scalar_only[1] ^= 1;
        let fresh = |s: &[u8; 32], p: &[u8; 32]| {
            let before = COMPUTED.load(Ordering::Relaxed);
            let got = seam::through(s, p, counted);
            (got, COMPUTED.load(Ordering::Relaxed) - before)
        };
        let first = fresh(&scalar, &point);
        assert!(first.0 == out && first.1 <= 1, "first: {first:?}");
        assert_eq!(fresh(&scalar, &point), (out, 0), "a repeat is the memo's");
        assert_eq!(fresh(&scalar, &other).0, ladder(&scalar, &other));
        assert_eq!(fresh(&scalar_only, &point).0, ladder(&scalar_only, &point));
    }
}
