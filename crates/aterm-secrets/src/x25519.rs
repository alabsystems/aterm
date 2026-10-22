// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! X25519 (RFC 7748 §5), first-party: TweetNaCl's `crypto_scalarmult` in the form proved
//! correct against RFC 7748 in Coq by Schwabe, Viguier, Weerwag and Wiedijk (CSF 2021,
//! ePrint 2021/428). See docs/DESIGN-secrets-2026-10-20.md §6 for why this is not ring's
//! (ring builds X25519 keys only from its own RNG) or dalek's (banned by deny.toml).
//!
//! Field elements are sixteen signed 64-bit limbs of radix 2^16; the ladder runs over bits
//! 254 down to 0 with masked conditional swaps, so neither branches nor memory addresses
//! depend on the scalar. The scalar is clamped as RFC 7748 §5 says, the input u-coordinate's
//! top bit is masked, and a non-canonical u (at or above p) is reduced. The caller checks
//! for the all-zero output (a low-order point).
//!
//! # Provenance, and what is (and is not) proved
//!
//! The functions below are a line-by-line transcription into Rust of the C in Appendix A of
//! "A Coq proof of the correctness of X25519 in TweetNaCl" (Schwabe, Viguier, Weerwag,
//! Wiedijk; IEEE CSF 2021; ePrint 2021/428): TweetNaCl's `crypto_scalarmult` with the
//! paper's changes applied, chiefly the `car25519` that has no undefined behaviour
//! (`o[(i+1)%16] += (i<15?1:38)*(o[i]>>16); o[i] &= 0xffff;`) and the final
//! `inv25519(c,c); M(a,a,c); pack25519(q,a)` in place of the original's scratch array.
//! Each Rust function keeps the C name it transcribes (`car25519`, `sel25519`, `pack25519`,
//! `unpack25519`, `inv25519`; `A`/`Z`/`M`/`S` are `add`/`sub`/`mul`/`sqr`) and the ladder
//! keeps the C's operation order and operand roles exactly.
//!
//! The paper's proof (in Coq, tied to the C by the Verified Software Toolchain) covers THAT
//! C code: that it runs without undefined behaviour (no overflow of the 64-bit limbs, no
//! array access out of bounds) and computes RFC 7748's X25519 for every 32-byte scalar and
//! u (the clamping, the masked top bit and the reduction of u mod p included), and that
//! this is scalar multiplication on Curve25519. The proof does not cover this Rust: the
//! transcription, Rust's semantics (the C's signed `>>` is arithmetic there as Rust's
//! `i64 >>` is; the C's freedom from overflow is what a debug build of this file would
//! report as a panic) and the compiler are checked here only by tests: every RFC 7748
//! §5.2 and §6.1 vector (the 1,000-iteration one in the default suite, the
//! 1,000,000-iteration one ignored by default), the low-order inputs, and a differential
//! suite against ring's X25519 on random keys. Constant time is a property of the code's
//! shape (no secret-dependent branch or index; swaps by mask), not of the proof, and no
//! tool here checks the machine code.
//!
//! The clamped scalar and the ladder's state are wiped before return (`zeroize`); the
//! temporaries the arithmetic returns by value are not individually wiped.

use zeroize::Zeroize;

/// The u-coordinate 9, the base point.
pub const BASEPOINT: [u8; 32] = {
    let mut point = [0u8; 32];
    point[0] = 9;
    point
};

/// A field element mod p = 2^255 - 19: sixteen signed limbs of radix 2^16 (TweetNaCl's `gf`).
type Gf = [i64; 16];

/// (A + 2) / 4 = 121665 for Curve25519 (TweetNaCl's `_121665`).
const A24: Gf = [0xDB41, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

/// X25519(scalar, u): the u-coordinate of the clamped scalar times the point with
/// u-coordinate `u`.
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

/// `car25519`: one carry pass, the top limb's carry folded into limb 0 times 38
/// (2^256 = 38 mod p). The verified form: no shift of a negative value left, no `c-1`.
#[inline]
fn car25519(o: &mut Gf) {
    for i in 0..16 {
        // The multiplier depends on the loop index only, never on the data.
        let fold: i64 = if i < 15 { 1 } else { 38 };
        o[(i + 1) % 16] += fold * (o[i] >> 16);
        o[i] &= 0xffff;
    }
}

/// `sel25519`: swaps `p` and `q` when `b` is 1, leaves them when `b` is 0, by mask: the
/// same instructions and addresses either way.
#[inline]
fn sel25519(p: &mut Gf, q: &mut Gf, b: i64) {
    let c = !(b - 1);
    for i in 0..16 {
        let t = c & (p[i] ^ q[i]);
        p[i] ^= t;
        q[i] ^= t;
    }
}

/// `pack25519`: the canonical little-endian 32 bytes of `n` (fully reduced mod p).
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

/// `unpack25519`: the limbs of a little-endian u-coordinate, its top bit masked.
fn unpack25519(n: &[u8; 32]) -> Gf {
    let mut o: Gf = [0; 16];
    for i in 0..16 {
        o[i] = i64::from(n[2 * i]) + (i64::from(n[2 * i + 1]) << 8);
    }
    o[15] &= 0x7fff;
    o
}

/// `A`: limb-wise sum, unreduced.
#[inline]
fn add(a: &Gf, b: &Gf) -> Gf {
    let mut o: Gf = [0; 16];
    for i in 0..16 {
        o[i] = a[i] + b[i];
    }
    o
}

/// `Z`: limb-wise difference, unreduced.
#[inline]
fn sub(a: &Gf, b: &Gf) -> Gf {
    let mut o: Gf = [0; 16];
    for i in 0..16 {
        o[i] = a[i] - b[i];
    }
    o
}

/// `M`: schoolbook product, the high half folded down times 38, two carry passes.
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

/// `S`: the square.
#[inline]
fn sqr(a: &Gf) -> Gf {
    mul(a, a)
}

/// `inv25519`: a^(p-2) by the fixed square-and-multiply chain (the exponent is public).
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

/// `crypto_scalarmult`: the Montgomery ladder over the clamped scalar's bits 254..=0.
fn crypto_scalarmult(n: &[u8; 32], p: &[u8; 32]) -> [u8; 32] {
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
        // The bit's position is public; only its value is secret, and it feeds masks.
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
                // u's last byte 0x93 has the top bit set: the ladder must mask it.
                "4b66e9d4d1b4673c5ad22691957d6af5c11b6421e0ea01d42ca4169e7918ba0d",
                "e5210f12786811d3f4b7959d0538ae2c31dbe7106fc03c3efc4cd549c715a493",
                "95cbde9476e8907d7aade45cb4b873f88b595a68799fa152e6f8f7647aac7957",
            ),
        ] {
            assert_eq!(x25519(&hex32(scalar), &hex32(u)), hex32(out));
        }
    }

    /// RFC 7748 §5.2's iterated test: k = u = 9; (k, u) = (X25519(k, u), k).
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

    /// The points of small order (and their non-canonical and top-bit-set spellings)
    /// give the all-zero output for every scalar: what age's "all-zero shared secret"
    /// refusal relies on.
    #[test]
    fn low_order_inputs_give_all_zero_output() {
        let mut points = vec![
            // 0 and 1.
            "0000000000000000000000000000000000000000000000000000000000000000",
            "0100000000000000000000000000000000000000000000000000000000000000",
            // The two points of order 8.
            "e0eb7a7c3b41b8ae1656e3faf19fc46ada098deb9c32b1fd866205165f49b800",
            "5f9c95bca3508c24b1d0b1559c83ef5b04445cc4581c8e86d8224eddd09f1157",
            // p - 1 (order 2), p (= 0) and p + 1 (= 1), non-canonical.
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

    /// A non-canonical u (u + p, which fits in 255 bits for u < 19) is reduced first.
    #[test]
    fn non_canonical_u_is_reduced() {
        let scalar = [7u8; 32];
        // 9 + p = 2^255 - 10.
        let nine_plus_p = hex32("f6ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f");
        assert_eq!(x25519(&scalar, &nine_plus_p), x25519_base(&scalar));
    }

    /// Differential: ring's X25519 (its keys come only from its own RNG) and this ladder
    /// agree on random keys in both directions.
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
}
