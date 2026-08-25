// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! SHA-256, transcribed from FIPS 180-4 section 6.2.
//!
//! No SIMD, no SHA-NI, no `unsafe`, no lookup trickery -- the reference
//! algorithm written so that a reader with the standard open beside them can
//! check every line. See the crate docs for why aterm carries this instead of
//! `sha2`.

/// The SHA-256 block size, in bytes (FIPS 180-4 sec. 1: 512 bits).
pub(crate) const BLOCK_LEN: usize = 64;

/// The SHA-256 digest size, in bytes.
pub(crate) const OUTPUT_LEN: usize = 32;

/// Initial hash value H^(0) -- FIPS 180-4 sec. 5.3.3.
///
/// The first thirty-two bits of the fractional parts of the square roots of
/// the first eight primes.
const H_INIT: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

/// Round constants K -- FIPS 180-4 sec. 4.2.2.
///
/// The first thirty-two bits of the fractional parts of the cube roots of the
/// first sixty-four primes.
#[rustfmt::skip]
const K: [u32; 64] = [
    0x428a_2f98, 0x7137_4491, 0xb5c0_fbcf, 0xe9b5_dba5,
    0x3956_c25b, 0x59f1_11f1, 0x923f_82a4, 0xab1c_5ed5,
    0xd807_aa98, 0x1283_5b01, 0x2431_85be, 0x550c_7dc3,
    0x72be_5d74, 0x80de_b1fe, 0x9bdc_06a7, 0xc19b_f174,
    0xe49b_69c1, 0xefbe_4786, 0x0fc1_9dc6, 0x240c_a1cc,
    0x2de9_2c6f, 0x4a74_84aa, 0x5cb0_a9dc, 0x76f9_88da,
    0x983e_5152, 0xa831_c66d, 0xb003_27c8, 0xbf59_7fc7,
    0xc6e0_0bf3, 0xd5a7_9147, 0x06ca_6351, 0x1429_2967,
    0x27b7_0a85, 0x2e1b_2138, 0x4d2c_6dfc, 0x5338_0d13,
    0x650a_7354, 0x766a_0abb, 0x81c2_c92e, 0x9272_2c85,
    0xa2bf_e8a1, 0xa81a_664b, 0xc24b_8b70, 0xc76c_51a3,
    0xd192_e819, 0xd699_0624, 0xf40e_3585, 0x106a_a070,
    0x19a4_c116, 0x1e37_6c08, 0x2748_774c, 0x34b0_bcb5,
    0x391c_0cb3, 0x4ed8_aa4a, 0x5b9c_ca4f, 0x682e_6ff3,
    0x748f_82ee, 0x78a5_636f, 0x84c8_7814, 0x8cc7_0208,
    0x90be_fffa, 0xa450_6ceb, 0xbef9_a3f7, 0xc671_78f2,
];

/// A streaming SHA-256 hasher.
///
/// Drop-in for `sha2::Sha256` across aterm's call sites: [`Sha256::new`],
/// [`Sha256::update`], [`Sha256::finalize`] and the one-shot
/// [`Sha256::digest`].
///
/// [`finalize`](Sha256::finalize) returns a plain `[u8; 32]` rather than a
/// `GenericArray`, which is what let the `digest`/`typenum` tower go. An array
/// derefs to `&[u8]`, so `hex(&h.finalize())` still compiles, and
/// `h.finalize().into()` still resolves through the identity `From`.
///
/// # Example
///
/// ```rust
/// use aterm_digest::Sha256;
///
/// let mut h = Sha256::new();
/// h.update(b"hello ");
/// h.update(b"world");
/// assert_eq!(h.finalize(), Sha256::digest(b"hello world"));
/// ```
#[derive(Clone)]
pub struct Sha256 {
    /// Working hash value H^(i).
    state: [u32; 8],
    /// Bytes not yet fed to the compression function; `buf[..buf_len]` is live.
    buf: [u8; BLOCK_LEN],
    /// How much of `buf` is live. Always `< BLOCK_LEN` between calls: `update`
    /// compresses the buffer the instant it fills, so `finalize` can always
    /// write its `0x80` byte without a bounds check.
    buf_len: usize,
    /// Total message length in BYTES.
    ///
    /// Counted in bytes, not bits, and widened to `u64` on the way in. Holding
    /// this in a `u32` (or counting bits in one) is the classic SHA
    /// implementation bug: it wraps silently at 512 MiB of input and produces a
    /// digest that disagrees with every other implementation on Earth. FIPS
    /// 180-4 caps a message at 2^64 - 1 bits, so the shift to bits in
    /// `finalize` cannot overflow for any message that is legal in the first
    /// place.
    len: u64,
}

impl Sha256 {
    /// Creates a hasher over the empty message.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: H_INIT,
            buf: [0u8; BLOCK_LEN],
            buf_len: 0,
            len: 0,
        }
    }

    /// Feeds more bytes into the hash.
    ///
    /// Splitting the same message across `update` calls in any pattern yields
    /// the same digest as one call with the whole message.
    pub fn update(&mut self, bytes: impl AsRef<[u8]>) {
        let mut data = bytes.as_ref();
        self.len = self.len.wrapping_add(data.len() as u64);

        // Top up a partial block left by a previous call, and compress it the
        // moment it is full -- keeping the `buf_len < BLOCK_LEN` invariant.
        if self.buf_len > 0 {
            let take = (BLOCK_LEN - self.buf_len).min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len < BLOCK_LEN {
                // `take` consumed all of `data` without filling the block, so
                // there is nothing else to do. Returning here is load-bearing:
                // falling through would run the tail-copy below with an empty
                // `data` and reset `buf_len` to 0, silently discarding the
                // bytes just buffered.
                debug_assert!(data.is_empty());
                return;
            }
            let block = self.buf;
            compress(&mut self.state, &block);
            self.buf_len = 0;
        }

        // Whole blocks stream straight out of the caller's slice: no copy into
        // `buf`, which is the difference between "simple" and "needlessly slow".
        let (blocks, rest) = data.as_chunks::<BLOCK_LEN>();
        for block in blocks {
            compress(&mut self.state, block);
        }

        // The tail (0..63 bytes) waits for the next call or for padding.
        self.buf[..rest.len()].copy_from_slice(rest);
        self.buf_len = rest.len();
    }

    /// Finishes the hash and returns the 32-byte digest.
    #[must_use]
    pub fn finalize(mut self) -> [u8; OUTPUT_LEN] {
        // FIPS 180-4 sec. 5.1.1 padding: a `1` bit, then `0` bits, then the
        // message length in bits as a big-endian u64, landing on a block
        // boundary.
        let bit_len = self.len.wrapping_mul(8);

        // `buf_len < BLOCK_LEN` on entry (see the field docs), so this index is
        // always in range.
        self.buf[self.buf_len] = 0x80;
        self.buf_len += 1;

        // If fewer than 8 bytes are left after the 0x80 the length field does
        // not fit, so the padding spills into a SECOND block: zero-fill and
        // compress this one, then lay the length into a fresh all-zero block.
        // A message of 56..=63 bytes mod 64 takes this path; getting it wrong
        // is the other classic SHA bug, and it hides until someone hashes a
        // 56-byte file.
        if self.buf_len > BLOCK_LEN - 8 {
            self.buf[self.buf_len..].fill(0);
            let block = self.buf;
            compress(&mut self.state, &block);
            self.buf_len = 0;
        }

        self.buf[self.buf_len..BLOCK_LEN - 8].fill(0);
        self.buf[BLOCK_LEN - 8..].copy_from_slice(&bit_len.to_be_bytes());
        let block = self.buf;
        compress(&mut self.state, &block);

        let mut out = [0u8; OUTPUT_LEN];
        let (words, _) = out.as_chunks_mut::<4>();
        for (slot, word) in words.iter_mut().zip(self.state.iter()) {
            *slot = word.to_be_bytes();
        }
        out
    }

    /// One-shot convenience: the SHA-256 digest of `bytes`.
    #[must_use]
    pub fn digest(bytes: impl AsRef<[u8]>) -> [u8; OUTPUT_LEN] {
        let mut h = Self::new();
        h.update(bytes);
        h.finalize()
    }
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Sha256 {
    /// Deliberately opaque: a hasher's interior is partially-consumed message
    /// material, which is exactly the thing that should not fall into a log
    /// line.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sha256").finish_non_exhaustive()
    }
}

/// The SHA-256 compression function -- FIPS 180-4 sec. 6.2.2.
///
/// Absorbs one 512-bit block into the eight-word state.
fn compress(state: &mut [u32; 8], block: &[u8; BLOCK_LEN]) {
    // 1. Prepare the message schedule W.
    let mut w = [0u32; 64];
    let (be_words, _) = block.as_chunks::<4>();
    for (word, chunk) in w[..16].iter_mut().zip(be_words) {
        *word = u32::from_be_bytes(*chunk);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }

    // 2. Initialise the working variables with the current hash value.
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;

    // 3. Sixty-four rounds.
    for (&kt, &wt) in K.iter().zip(w.iter()) {
        let big_s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ (!e & g);
        let t1 = h
            .wrapping_add(big_s1)
            .wrapping_add(ch)
            .wrapping_add(kt)
            .wrapping_add(wt);
        let big_s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = big_s0.wrapping_add(maj);

        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }

    // 4. Compute the next hash value.
    let round = [a, b, c, d, e, f, g, h];
    for (s, r) in state.iter_mut().zip(round.iter()) {
        *s = s.wrapping_add(*r);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hex;

    // -----------------------------------------------------------------------
    // FIPS 180-4 / NIST published vectors
    // -----------------------------------------------------------------------

    #[test]
    fn fips_empty_message() {
        assert_eq!(
            hex(&Sha256::digest(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn fips_abc() {
        // FIPS 180-4 appendix B.1: a one-block message.
        assert_eq!(
            hex(&Sha256::digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn fips_448_bit_message() {
        // FIPS 180-4 appendix B.2: 56 bytes -- the padding lands with exactly
        // 8 bytes free, the tightest single-block case.
        let msg = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
        assert_eq!(msg.len(), 56);
        assert_eq!(
            hex(&Sha256::digest(msg)),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn fips_896_bit_message() {
        // FIPS 180-4 appendix B.3: 112 bytes, two blocks plus a padding block.
        let msg = b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu";
        assert_eq!(msg.len(), 112);
        assert_eq!(
            hex(&Sha256::digest(msg)),
            "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1"
        );
    }

    #[test]
    fn fips_one_million_a() {
        // FIPS 180-4 appendix B.3 long message: 1,000,000 'a'. Exercises the
        // byte counter well past a single block and past 2^16 bytes.
        let msg = vec![b'a'; 1_000_000];
        assert_eq!(
            hex(&Sha256::digest(&msg)),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    // -----------------------------------------------------------------------
    // Padding edge cases around the block boundary
    // -----------------------------------------------------------------------

    #[test]
    fn padding_boundary_lengths_stream_identically() {
        // 55 fits the length field; 56..=63 and 64 force a second padding
        // block; 64 is also an exact block. Each must agree with the digest
        // reached by feeding one byte at a time.
        for len in [0usize, 1, 54, 55, 56, 57, 63, 64, 65, 119, 120, 127, 128] {
            let msg: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let one_shot = Sha256::digest(&msg);

            let mut drip = Sha256::new();
            for byte in &msg {
                drip.update([*byte]);
            }
            assert_eq!(one_shot, drip.finalize(), "length {len}");
        }
    }

    #[test]
    fn the_length_field_is_64_bits_wide() {
        // 4 GiB of message is 2^35 bits -- everything above bit 31 of the
        // length field. Only the padding block depends on `len`, so this pins
        // the encoding without moving 4 GiB of bytes. Without it, narrowing
        // `bit_len` to a u32 passes the ENTIRE suite (mutation-checked) while
        // disagreeing with every other SHA-256 on any input over 512 MiB --
        // which `aterm_release::dmg::sha256_file` hashes on every cut.
        let mut h = Sha256::new();
        h.len = 1 << 32;
        let got = h.finalize();

        let mut padding = [0u8; BLOCK_LEN];
        padding[0] = 0x80;
        padding[BLOCK_LEN - 8..].copy_from_slice(&(1u64 << 35).to_be_bytes());
        let mut state = H_INIT;
        compress(&mut state, &padding);
        let mut want = [0u8; OUTPUT_LEN];
        for (i, word) in state.iter().enumerate() {
            want[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        assert_eq!(got, want);
    }

    #[test]
    fn default_matches_new() {
        assert_eq!(Sha256::default().finalize(), Sha256::new().finalize());
    }

    #[test]
    fn debug_does_not_leak_state() {
        let mut h = Sha256::new();
        h.update(b"a secret that must not appear in a log");
        let rendered = format!("{h:?}");
        assert!(!rendered.contains("secret"), "{rendered}");
        assert!(rendered.starts_with("Sha256"), "{rendered}");
    }

    #[test]
    fn clone_forks_the_hash_state() {
        let mut h = Sha256::new();
        h.update(b"prefix ");
        let mut a = h.clone();
        let mut b = h;
        a.update(b"one");
        b.update(b"two");
        assert_eq!(a.finalize(), Sha256::digest(b"prefix one"));
        assert_eq!(b.finalize(), Sha256::digest(b"prefix two"));
    }
}
