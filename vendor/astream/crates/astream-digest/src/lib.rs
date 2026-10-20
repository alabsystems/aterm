#![forbid(unsafe_code)]
//! `astream-digest` — SHA-256, transcribed from FIPS 180-4 section 6.2: the
//! reference algorithm, checkable line by line against the standard.
//!
//! It is the one hash in the workspace. `astream-cap` builds its RFC 2104
//! HMAC-SHA256 (capability tags, attach proofs) and derives producer ids over it;
//! `astream-aead`'s handshake builds HMAC and HKDF over it; `astream-evidence`
//! pins command output with it. Each of those owns its construction; this crate
//! owns only the primitive.
//!
//! Zero dependencies and no `unsafe`, SIMD or assembly: astream hashes grants,
//! nonces and test output, not bulk traffic. The const slice updater copies bytes
//! into one 64-byte buffer and compresses it whenever it fills.
//!
//! Ported from aterm's `aterm-digest` (the same author; it now re-exports this one),
//! with its tests. Checked against the FIPS 180-4 vectors below, and against the
//! `sha2` crate it replaced: every length 0..=1024, whole and under random update
//! splits, matched sha2 byte for byte when it landed, and
//! `tests/lengths_and_splits.rs` pins the answers sha2 gave.
//!
//! ```
//! use astream_digest::Sha256;
//!
//! // One-shot.
//! let d = Sha256::digest(b"abc");
//! assert_eq!(d[0], 0xba);
//!
//! // Streaming: any split of the same bytes answers the same digest.
//! let mut h = Sha256::new();
//! h.update(b"a");
//! h.update(b"bc");
//! assert_eq!(h.finalize(), d);
//! ```

// A contracted fn is written once, through `contract_fn!`: under a Trust compiler
// (`cfg(clean_islands)`, build.rs) it carries its `ensures` clause, which no stock rustc can
// parse, and every other compiler gets the plain fn.
#[macro_use]
#[path = "../../astream-wire/contract_fn.rs"]
mod contract_fn;

// Plain-Rust twins of the functions this crate's Clean islands compile, for a compiler
// that cannot lex the islands.
#[cfg(any(test, not(clean_islands)))]
mod twins;

/// The SHA-256 block size, in bytes (FIPS 180-4 sec. 1: 512 bits) — also the
/// HMAC block size `B` of RFC 2104.
pub const BLOCK_LEN: usize = 64;

/// Maximum partial-block byte count (`BLOCK_LEN - 1 == 63`).
const MAX_PARTIAL_LEN: usize = 63;

/// Byte offset of the 64-bit length field in the final padded block (`BLOCK_LEN - 8 == 56`).
const PAD_LEN_OFFSET: usize = 56;

/// The SHA-256 digest size, in bytes.
pub const OUTPUT_LEN: usize = 32;

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

/// Multi-parameter streaming SHA-256 buffer occupancy transition kernel:
/// computes the next partial-buffer length in `0..64` after feeding `data_len`
/// bytes with post-chunk remainder `tail_len`. Matches the Clean kernel specification
/// `Astream.Digest.TlaStreamingSha256.sha256_next_buf_len_isl`.
#[must_use]
pub const fn sha256_next_buf_len(cur_buf_len: u64, data_len: u64, tail_len: u64) -> u64 {
    let clamped_tail = if tail_len < 63 { tail_len } else { 63 };
    if cur_buf_len == 0 {
        clamped_tail
    } else {
        let raw_cur = if cur_buf_len < 63 { cur_buf_len } else { 63 };
        let clamped_cur = raw_cur & 63;
        // For `clamped_cur` in `0..=63`, `63 - clamped_cur == (clamped_cur ^ 63) & 63`,
        // so `64 - clamped_cur == ((clamped_cur ^ 63) & 63) + 1`.
        let avail = ((clamped_cur ^ 63) & 63) + 1;
        let raw_take = if data_len < avail { data_len } else { avail };
        let take = raw_take & 127;
        let filled = clamped_cur + take;
        if filled < 64 {
            filled
        } else {
            clamped_tail
        }
    }
}

contract_fn! {
    /// Pure partial-block invariant check (`buf_len < block_len`), verified by
    /// `clean-kernel-defeq` against `buf_len_is_partial_isl` in the Clean TLA+ island.
    #[must_use]
    pub const fn buf_len_is_partial(buf_len: u64, block_len: u64) -> bool
    clauses [ensures result == (buf_len < block_len)]
    {
        buf_len < block_len
    }
}

// The Clean island, out of line: only a Trust compiler lexes it, so it mounts under
// `cfg(clean_islands)` (build.rs) and a stock compiler never opens the file.
#[cfg(clean_islands)]
mod digest_island;
#[cfg(not(clean_islands))]
pub use crate::twins::{sha256_clamp_partial_len, sha256_partial_take};
#[cfg(clean_islands)]
pub use digest_island::{sha256_clamp_partial_len, sha256_partial_take};

/// A streaming SHA-256 hasher.
///
/// [`finalize`](Sha256::finalize) returns a plain `[u8; 32]`. Fed in pieces or
/// whole, it answers the same digest (the tests below hold it at every padding
/// boundary).
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
    /// Total message length in bytes, as `u64`. A `u32` (or a bit count in
    /// one) would wrap at 512 MiB; FIPS 180-4 caps a message at 2^64 - 1
    /// bits, so the shift to bits in `finalize` cannot overflow for a legal
    /// message.
    len: u64,
}

impl Sha256 {
    /// Creates a hasher over the empty message.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: H_INIT,
            buf: [0u8; BLOCK_LEN],
            buf_len: 0,
            len: 0,
        }
    }

    /// Feeds a byte slice into the hash without generic `AsRef` dispatch.
    pub const fn update_slice(&mut self, mut data: &[u8]) {
        let data_len = data.len() as u64;
        self.len = self.len.wrapping_add(data_len);
        let mut cur = if buf_len_is_partial(self.buf_len as u64, BLOCK_LEN as u64) {
            self.buf_len & MAX_PARTIAL_LEN
        } else {
            0
        };
        while let &[b, ref rest @ ..] = data {
            self.buf[cur & MAX_PARTIAL_LEN] = b;
            cur = (cur & MAX_PARTIAL_LEN) + 1;
            if cur == BLOCK_LEN {
                let block = self.buf;
                compress(&mut self.state, &block);
                cur = 0;
            }
            data = rest;
        }
        self.buf_len = cur;
    }

    /// Feeds more bytes into the hash.
    ///
    /// Splitting the same message across `update` calls in any pattern yields
    /// the same digest as one call with the whole message.
    pub fn update(&mut self, bytes: impl AsRef<[u8]>) {
        self.update_slice(bytes.as_ref());
    }

    /// Finishes the hash and returns the 32-byte digest.
    #[must_use]
    pub const fn finalize(mut self) -> [u8; OUTPUT_LEN] {
        // FIPS 180-4 sec. 5.1.1 padding: a `1` bit, then `0` bits, then the
        // message length in bits as a big-endian u64, landing on a block
        // boundary.
        let bit_len = self.len.wrapping_mul(8);

        // `buf_len < BLOCK_LEN` on entry (see the field docs), so this index is
        // always in range.
        let mut pos = self.buf_len & MAX_PARTIAL_LEN;
        self.buf[pos] = 0x80;
        pos += 1;

        // With fewer than 8 bytes left after the 0x80, the length field spills
        // into a second block: zero-fill and compress this one, then lay the
        // length into a fresh zero block (messages of 56..=63 bytes mod 64).
        if pos > PAD_LEN_OFFSET {
            while pos < BLOCK_LEN {
                self.buf[pos & MAX_PARTIAL_LEN] = 0;
                pos += 1;
            }
            let block = self.buf;
            compress(&mut self.state, &block);
            pos = 0;
        }

        while pos < PAD_LEN_OFFSET {
            self.buf[pos & MAX_PARTIAL_LEN] = 0;
            pos += 1;
        }
        write_pad_len_be(&mut self.buf, bit_len);
        self.buf_len = 0;
        let block = self.buf;
        compress(&mut self.state, &block);

        encode_state_be(&self.state)
    }

    /// One-shot SHA-256 digest of a byte slice without generic `AsRef` dispatch.
    #[must_use]
    pub const fn digest_slice(bytes: &[u8]) -> [u8; OUTPUT_LEN] {
        let mut h = Self::new();
        h.update_slice(bytes);
        h.finalize()
    }

    /// One-shot convenience: the SHA-256 digest of `bytes`.
    #[must_use]
    pub fn digest(bytes: impl AsRef<[u8]>) -> [u8; OUTPUT_LEN] {
        Self::digest_slice(bytes.as_ref())
    }
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Sha256 {
    /// Opaque: a hasher's interior is partially consumed message material — a
    /// capability secret, under HMAC — which must not reach a log line.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sha256").finish_non_exhaustive()
    }
}

const fn write_pad_len_be(buf: &mut [u8; BLOCK_LEN], bit_len: u64) {
    buf[56] = ((bit_len >> 56) & 0xff) as u8;
    buf[57] = ((bit_len >> 48) & 0xff) as u8;
    buf[58] = ((bit_len >> 40) & 0xff) as u8;
    buf[59] = ((bit_len >> 32) & 0xff) as u8;
    buf[60] = ((bit_len >> 24) & 0xff) as u8;
    buf[61] = ((bit_len >> 16) & 0xff) as u8;
    buf[62] = ((bit_len >> 8) & 0xff) as u8;
    buf[63] = (bit_len & 0xff) as u8;
}

const fn encode_state_be(state: &[u32; 8]) -> [u8; OUTPUT_LEN] {
    let s0 = state[0];
    let s1 = state[1];
    let s2 = state[2];
    let s3 = state[3];
    let s4 = state[4];
    let s5 = state[5];
    let s6 = state[6];
    let s7 = state[7];
    [
        ((s0 >> 24) & 0xff) as u8,
        ((s0 >> 16) & 0xff) as u8,
        ((s0 >> 8) & 0xff) as u8,
        (s0 & 0xff) as u8,
        ((s1 >> 24) & 0xff) as u8,
        ((s1 >> 16) & 0xff) as u8,
        ((s1 >> 8) & 0xff) as u8,
        (s1 & 0xff) as u8,
        ((s2 >> 24) & 0xff) as u8,
        ((s2 >> 16) & 0xff) as u8,
        ((s2 >> 8) & 0xff) as u8,
        (s2 & 0xff) as u8,
        ((s3 >> 24) & 0xff) as u8,
        ((s3 >> 16) & 0xff) as u8,
        ((s3 >> 8) & 0xff) as u8,
        (s3 & 0xff) as u8,
        ((s4 >> 24) & 0xff) as u8,
        ((s4 >> 16) & 0xff) as u8,
        ((s4 >> 8) & 0xff) as u8,
        (s4 & 0xff) as u8,
        ((s5 >> 24) & 0xff) as u8,
        ((s5 >> 16) & 0xff) as u8,
        ((s5 >> 8) & 0xff) as u8,
        (s5 & 0xff) as u8,
        ((s6 >> 24) & 0xff) as u8,
        ((s6 >> 16) & 0xff) as u8,
        ((s6 >> 8) & 0xff) as u8,
        (s6 & 0xff) as u8,
        ((s7 >> 24) & 0xff) as u8,
        ((s7 >> 16) & 0xff) as u8,
        ((s7 >> 8) & 0xff) as u8,
        (s7 & 0xff) as u8,
    ]
}

const fn sha256_sigma0_small(x: u32) -> u32 {
    x.rotate_right(7) ^ x.rotate_right(18) ^ (x >> 3)
}

const fn sha256_sigma1_small(x: u32) -> u32 {
    x.rotate_right(17) ^ x.rotate_right(19) ^ (x >> 10)
}

const fn sha256_schedule_word(w16: u32, w15: u32, w7: u32, w2: u32) -> u32 {
    w16.wrapping_add(sha256_sigma0_small(w15))
        .wrapping_add(w7)
        .wrapping_add(sha256_sigma1_small(w2))
}

const fn sha256_ch(e: u32, f: u32, g: u32) -> u32 {
    (e & f) ^ (!e & g)
}

const fn sha256_maj(a: u32, b: u32, c: u32) -> u32 {
    (a & b) ^ (a & c) ^ (b & c)
}

const fn add_state_words(state: &mut [u32; 8], work: &[u32; 8]) {
    state[0] = state[0].wrapping_add(work[0]);
    state[1] = state[1].wrapping_add(work[1]);
    state[2] = state[2].wrapping_add(work[2]);
    state[3] = state[3].wrapping_add(work[3]);
    state[4] = state[4].wrapping_add(work[4]);
    state[5] = state[5].wrapping_add(work[5]);
    state[6] = state[6].wrapping_add(work[6]);
    state[7] = state[7].wrapping_add(work[7]);
}

const fn load_be_u32(bytes: &[u8; 4]) -> u32 {
    ((bytes[0] as u32) << 24)
        | ((bytes[1] as u32) << 16)
        | ((bytes[2] as u32) << 8)
        | (bytes[3] as u32)
}

/// The SHA-256 compression function -- FIPS 180-4 sec. 6.2.2.
///
/// Absorbs one 512-bit block into the eight-word state.
const fn compress(state: &mut [u32; 8], block: &[u8; BLOCK_LEN]) {
    // 1. Prepare the message schedule W.
    let mut w = [0u32; 64];
    w[0] = load_be_u32(&[block[0], block[1], block[2], block[3]]);
    w[1] = load_be_u32(&[block[4], block[5], block[6], block[7]]);
    w[2] = load_be_u32(&[block[8], block[9], block[10], block[11]]);
    w[3] = load_be_u32(&[block[12], block[13], block[14], block[15]]);
    w[4] = load_be_u32(&[block[16], block[17], block[18], block[19]]);
    w[5] = load_be_u32(&[block[20], block[21], block[22], block[23]]);
    w[6] = load_be_u32(&[block[24], block[25], block[26], block[27]]);
    w[7] = load_be_u32(&[block[28], block[29], block[30], block[31]]);
    w[8] = load_be_u32(&[block[32], block[33], block[34], block[35]]);
    w[9] = load_be_u32(&[block[36], block[37], block[38], block[39]]);
    w[10] = load_be_u32(&[block[40], block[41], block[42], block[43]]);
    w[11] = load_be_u32(&[block[44], block[45], block[46], block[47]]);
    w[12] = load_be_u32(&[block[48], block[49], block[50], block[51]]);
    w[13] = load_be_u32(&[block[52], block[53], block[54], block[55]]);
    w[14] = load_be_u32(&[block[56], block[57], block[58], block[59]]);
    w[15] = load_be_u32(&[block[60], block[61], block[62], block[63]]);
    let mut i = 16usize;
    while i < 64 {
        if i >= 16 {
            let w15 = w[(i - 15) & 63];
            let w2 = w[(i - 2) & 63];
            let w16 = w[(i - 16) & 63];
            let w7 = w[(i - 7) & 63];
            w[i & 63] = sha256_schedule_word(w16, w15, w7, w2);
        }
        i += 1;
    }

    // 2. Initialise the working variables with the current hash value.
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;

    // 3. Sixty-four rounds.
    let mut round = 0usize;
    while round < 64 {
        let kt = K[round & 63];
        let wt = w[round & 63];
        // Sigma_1(e), Ch(e, f, g) -- eqs. (4.7), (4.2)
        let big_s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = sha256_ch(e, f, g);
        let t1 = h
            .wrapping_add(big_s1)
            .wrapping_add(ch)
            .wrapping_add(kt)
            .wrapping_add(wt);

        // Sigma_0(a), Maj(a, b, c) -- eqs. (4.6), (4.3)
        let big_s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = sha256_maj(a, b, c);
        let t2 = big_s0.wrapping_add(maj);

        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
        round += 1;
    }

    // 4. Compute the next hash value.
    add_state_words(state, &[a, b, c, d, e, f, g, h]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

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
        // 55 fits the length field; 56..=63 spill the length into a second
        // padding block; 64 is an exact block followed by one padding-only
        // block. Each must agree with the digest reached by feeding one byte
        // at a time.
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
        // 4 GiB of message is 2^35 bits, exercising the length field above bit
        // 31. Only the padding block depends on `len`, so this pins the
        // encoding without hashing 4 GiB.
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

    #[test]
    fn clean_origin_streaming_sha256_kernels_enforce_tla_invariants() {
        assert!(buf_len_is_partial(0, BLOCK_LEN as u64));
        assert!(buf_len_is_partial(63, BLOCK_LEN as u64));
        assert!(!buf_len_is_partial(64, BLOCK_LEN as u64));
        assert_eq!(sha256_partial_take(44, 15), 15);
        assert_eq!(sha256_partial_take(44, 100), 44);
        assert_eq!(sha256_clamp_partial_len(12, 63), 12);
        assert_eq!(sha256_clamp_partial_len(99, 63), 63);
        assert_eq!(sha256_next_buf_len(0, 100, 36), 36);
        assert_eq!(sha256_next_buf_len(20, 15, 0), 35);
        assert_eq!(sha256_next_buf_len(20, 100, 12), 12);
    }
}
