// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The test oracle: the X25519 ladder and Elligator 2 map that `x25519.rs` ran before its
//! radix-2^51 field, operation for operation (only its wipes are gone: it sees test inputs
//! alone). It is TweetNaCl's `crypto_scalarmult` in the form
//! Schwabe, Viguier, Weerwag and Wiedijk proved correct against RFC 7748 in Coq (CSF 2021,
//! ePrint 2021/428): sixteen 16-bit limbs in `i64`, the UB-free `car25519`, masked `sel25519`
//! swaps, and `inv25519` by square and multiply over p - 2. It shares no limb code with the field
//! it checks, so the two agree only where both compute GF(2^255 - 19).

/// A TweetNaCl field element: sixteen limbs of 16 bits, signed so that a difference may go
/// below zero until the next carry.
pub(super) type Gf = [i64; 16];

const A24: Gf = [0xDB41, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

/// curve25519's Montgomery coefficient `A` = 486662 = 0x76d06 (RFC 7748 §4.1).
pub(super) const A: Gf = [0x6D06, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

pub(super) const ZERO: Gf = [0; 16];

pub(super) const ONE: Gf = [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

/// One carry pass. This and [`mul`] wrap as a release build does (main's c24d52763e): the Coq
/// proof bounds every limb of the ladder, so a wrap is never reached there, and a checked build
/// spent most of a ladder's instructions on these two functions' checks.
fn car25519(o: &mut Gf) {
    for i in 0..16 {
        let fold: i64 = if i < 15 { 1 } else { 38 };
        let next = (i + 1) % 16;
        o[next] = o[next].wrapping_add(fold.wrapping_mul(o[i] >> 16));
        o[i] &= 0xffff;
    }
}

fn sel25519(p: &mut Gf, q: &mut Gf, b: i64) {
    let c = std::hint::black_box(!(b - 1));
    for i in 0..16 {
        let t = c & (p[i] ^ q[i]);
        p[i] ^= t;
        q[i] ^= t;
    }
}

pub(super) fn pack25519(n: &Gf) -> [u8; 32] {
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
    o
}

pub(super) fn unpack25519(n: &[u8; 32]) -> Gf {
    let mut o: Gf = [0; 16];
    for i in 0..16 {
        o[i] = i64::from(n[2 * i]) + (i64::from(n[2 * i + 1]) << 8);
    }
    o[15] &= 0x7fff;
    o
}

pub(super) fn add(a: &Gf, b: &Gf) -> Gf {
    let mut o: Gf = [0; 16];
    for i in 0..16 {
        o[i] = a[i] + b[i];
    }
    o
}

pub(super) fn sub(a: &Gf, b: &Gf) -> Gf {
    let mut o: Gf = [0; 16];
    for i in 0..16 {
        o[i] = a[i] - b[i];
    }
    o
}

pub(super) fn mul(a: &Gf, b: &Gf) -> Gf {
    let mut t = [0i64; 31];
    for i in 0..16 {
        for j in 0..16 {
            let product = a[i].wrapping_mul(b[j]);
            t[i + j] = t[i + j].wrapping_add(product);
        }
    }
    for i in 0..15 {
        let fold = 38i64.wrapping_mul(t[i + 16]);
        t[i] = t[i].wrapping_add(fold);
    }
    let mut o: Gf = [0; 16];
    o.copy_from_slice(&t[..16]);
    car25519(&mut o);
    car25519(&mut o);
    o
}

pub(super) fn sqr(a: &Gf) -> Gf {
    mul(a, a)
}

pub(super) fn inv25519(i: &Gf) -> Gf {
    let mut c: Gf = *i;
    for a in (0..=253).rev() {
        c = sqr(&c);
        if a != 2 && a != 4 {
            c = mul(&c, i);
        }
    }
    c
}

fn select(a: &Gf, b: &Gf, bit: i64) -> Gf {
    let mask = std::hint::black_box(-bit);
    let mut o: Gf = *a;
    for i in 0..16 {
        o[i] ^= mask & (a[i] ^ b[i]);
    }
    o
}

pub(super) fn equal(a: &Gf, b: &Gf) -> i64 {
    let x = pack25519(a);
    let y = pack25519(b);
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= x[i] ^ y[i];
    }
    std::hint::black_box(((i64::from(std::hint::black_box(diff)) - 1) >> 8) & 1)
}

pub(super) fn legendre(i: &Gf) -> Gf {
    let mut c: Gf = *i;
    for a in (0..=252).rev() {
        c = sqr(&c);
        if a != 3 && a != 0 {
            c = mul(&c, i);
        }
    }
    c
}

pub(super) fn is_square(i: &Gf) -> i64 {
    let l = legendre(i);
    equal(&l, &ONE) | equal(&l, &ZERO)
}

pub(super) fn elligator2(r: &[u8; 32]) -> [u8; 32] {
    let u = unpack25519(r);
    let mut tv1 = sqr(&u);
    tv1 = add(&tv1, &tv1);
    let x1 = mul(&sub(&ZERO, &A), &inv25519(&add(&tv1, &ONE)));
    let gx1 = mul(&add(&mul(&add(&x1, &A), &x1), &ONE), &x1);
    let x2 = mul(&sub(&sub(&ZERO, &x1), &A), &ONE);
    pack25519(&select(&x2, &x1, is_square(&gx1)))
}

pub(super) fn ladder(n: &[u8; 32], p: &[u8; 32]) -> [u8; 32] {
    let mut z: [u8; 32] = *n;
    z[31] = (n[31] & 127) | 64;
    z[0] &= 248;
    let x = unpack25519(p);
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
    }
    c = inv25519(&c);
    a = mul(&a, &c);
    pack25519(&a)
}
