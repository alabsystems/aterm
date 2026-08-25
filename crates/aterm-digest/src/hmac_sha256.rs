// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! HMAC-SHA256, transcribed from RFC 2104.
//!
//! `HMAC(K, m) = H((K' ^ opad) || H((K' ^ ipad) || m))`, where `K'` is the key
//! zero-padded to the hash's block size, or `H(K)` zero-padded when the key is
//! longer than a block.

use crate::ct_eq;
use crate::sha256::{BLOCK_LEN, OUTPUT_LEN, Sha256};

/// Inner padding byte -- RFC 2104 sec. 2.
const IPAD: u8 = 0x36;

/// Outer padding byte -- RFC 2104 sec. 2.
const OPAD: u8 = 0x5c;

/// Returned by [`HmacSha256::new_from_slice`] for API compatibility with the
/// `hmac` crate.
///
/// HMAC accepts a key of **any** length by construction, so this crate never
/// actually produces one -- long keys are hashed down, short keys are
/// zero-padded. The type exists so call sites keep their
/// `.expect("HMAC-SHA256 accepts a key of any length")` and read the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidLength;

impl std::fmt::Display for InvalidLength {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid key length")
    }
}

impl std::error::Error for InvalidLength {}

/// A MAC did not verify.
///
/// Deliberately carries nothing. Reporting *how* a tag was wrong -- which byte,
/// what length -- is the same oracle [`ct_eq`] exists to close.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacError;

impl std::fmt::Display for MacError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MAC tag mismatch")
    }
}

impl std::error::Error for MacError {}

/// HMAC-SHA256 (RFC 2104), keyed with SHA-256.
///
/// Drop-in for `hmac::Hmac<sha2::Sha256>` across aterm's call sites, except
/// that [`finalize`](HmacSha256::finalize) hands back a plain `[u8; 32]`
/// instead of a `CtOutput`, so a call site that wrote
/// `mac.finalize().into_bytes()` drops the `.into_bytes()`.
///
/// # Example
///
/// ```rust
/// use aterm_digest::HmacSha256;
///
/// let mut mac = HmacSha256::new_from_slice(b"shared secret")
///     .expect("HMAC-SHA256 accepts a key of any length");
/// mac.update(b"authenticated data");
/// let tag = mac.finalize();
///
/// let mut verifier = HmacSha256::new_from_slice(b"shared secret")
///     .expect("HMAC-SHA256 accepts a key of any length");
/// verifier.update(b"authenticated data");
/// assert!(verifier.verify_slice(&tag).is_ok());
/// ```
#[derive(Clone)]
pub struct HmacSha256 {
    /// `H((K' ^ ipad) || m)` in progress -- already primed with the ipad block.
    inner: Sha256,
    /// `K' ^ opad`, held until the outer hash is run at finalize time.
    outer_key: [u8; BLOCK_LEN],
}

impl HmacSha256 {
    /// Creates a MAC over the given key.
    ///
    /// Every key length is accepted, per RFC 2104: keys longer than the
    /// 64-byte block are replaced by their SHA-256 digest, shorter keys are
    /// zero-padded to the block. The `Result` is API compatibility with the
    /// `hmac` crate and is always `Ok`.
    ///
    /// # Errors
    ///
    /// Never. See [`InvalidLength`].
    pub fn new_from_slice(key: &[u8]) -> Result<Self, InvalidLength> {
        // K' -- the key reduced to exactly one block.
        let mut padded = [0u8; BLOCK_LEN];
        if key.len() > BLOCK_LEN {
            padded[..OUTPUT_LEN].copy_from_slice(&Sha256::digest(key));
        } else {
            padded[..key.len()].copy_from_slice(key);
        }

        let mut ipad_block = [0u8; BLOCK_LEN];
        let mut outer_key = [0u8; BLOCK_LEN];
        for ((i, o), &k) in ipad_block
            .iter_mut()
            .zip(outer_key.iter_mut())
            .zip(padded.iter())
        {
            *i = k ^ IPAD;
            *o = k ^ OPAD;
        }

        let mut inner = Sha256::new();
        inner.update(ipad_block);
        Ok(Self { inner, outer_key })
    }

    /// Feeds more bytes into the authenticated message.
    pub fn update(&mut self, bytes: &[u8]) {
        self.inner.update(bytes);
    }

    /// Finishes the MAC and returns the 32-byte tag.
    #[must_use]
    pub fn finalize(self) -> [u8; OUTPUT_LEN] {
        let inner = self.inner.finalize();
        let mut outer = Sha256::new();
        outer.update(self.outer_key);
        outer.update(inner);
        outer.finalize()
    }

    /// Checks `tag` against the computed MAC in constant time.
    ///
    /// The comparison runs through [`ct_eq`], which folds every byte rather
    /// than returning at the first mismatch. Comparing tags with `==` here
    /// would hand an attacker a byte-at-a-time forgery oracle; see the [`ct_eq`]
    /// docs for the full argument.
    ///
    /// # Errors
    ///
    /// [`MacError`] when `tag` is not the MAC of the message fed in, including
    /// when it is the right bytes at the wrong length (a truncated tag never
    /// verifies).
    pub fn verify_slice(self, tag: &[u8]) -> Result<(), MacError> {
        if ct_eq(&self.finalize(), tag) {
            Ok(())
        } else {
            Err(MacError)
        }
    }
}

impl std::fmt::Debug for HmacSha256 {
    /// Opaque on purpose: the struct holds `K' ^ opad`, from which the key is
    /// recoverable by XOR. That must never reach a log line.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HmacSha256").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{hex, unhex};

    fn mac(key: &[u8], data: &[u8]) -> [u8; OUTPUT_LEN] {
        let mut m = HmacSha256::new_from_slice(key).expect("HMAC-SHA256 accepts any key length");
        m.update(data);
        m.finalize()
    }

    // -----------------------------------------------------------------------
    // RFC 4231 test vectors for HMAC-SHA256
    // -----------------------------------------------------------------------

    #[test]
    fn rfc4231_case_1() {
        let key = [0x0b; 20];
        assert_eq!(
            hex(&mac(&key, b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn rfc4231_case_2() {
        // A key SHORTER than the digest, exercising the zero-pad path.
        assert_eq!(
            hex(&mac(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn rfc4231_case_3() {
        let key = [0xaa; 20];
        let data = [0xdd; 50];
        assert_eq!(
            hex(&mac(&key, &data)),
            "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe"
        );
    }

    #[test]
    fn rfc4231_case_4() {
        let key: Vec<u8> = (0x01u8..=0x19).collect();
        assert_eq!(key.len(), 25);
        let data = [0xcd; 50];
        assert_eq!(
            hex(&mac(&key, &data)),
            "82558a389a443c0ea4cc819899f2083a85f0faa3e578f8077a2e3ff46729665b"
        );
    }

    #[test]
    fn rfc4231_case_5() {
        // RFC 4231 publishes case 5 truncated to 128 bits.
        let key = [0x0c; 20];
        let tag = mac(&key, b"Test With Truncation");
        assert_eq!(hex(&tag[..16]), "a3b6167473100ee06e0c796c2955552b");
    }

    #[test]
    fn rfc4231_case_6() {
        // 131-byte key: LONGER than the 64-byte block, so it is hashed first.
        let key = [0xaa; 131];
        assert_eq!(
            hex(&mac(
                &key,
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn rfc4231_case_7() {
        // Over-block-size key AND over-block-size data.
        let key = [0xaa; 131];
        let data: &[u8] = b"This is a test using a larger than block-size key and a larger than block-size data. The key needs to be hashed before being used by the HMAC algorithm.";
        assert_eq!(data.len(), 152);
        assert_eq!(
            hex(&mac(&key, data)),
            "9b09ffa71b942fcb27635fbcd5b0e944bfdc63644f0713938a7f51535c3a35e2"
        );
    }

    // -----------------------------------------------------------------------
    // Key handling
    // -----------------------------------------------------------------------

    #[test]
    fn every_key_length_is_accepted() {
        // The call sites assert exactly this with
        // `.expect("HMAC-SHA256 accepts a key of any length")`.
        for len in [0usize, 1, 31, 32, 33, 63, 64, 65, 127, 128, 1000] {
            let key = vec![0x5au8; len];
            assert!(
                HmacSha256::new_from_slice(&key).is_ok(),
                "key length {len} rejected"
            );
        }
    }

    #[test]
    fn exactly_block_size_key_is_not_hashed() {
        // A 64-byte key is used verbatim; a 65-byte key is hashed. The two must
        // therefore differ even when the 65th byte is the zero that padding
        // would have supplied.
        let mut k64 = [0u8; 64];
        k64[0] = 0x91;
        let mut k65 = [0u8; 65];
        k65[0] = 0x91;
        assert_ne!(mac(&k64, b"m"), mac(&k65, b"m"));

        // And a long key equals the MAC under its own digest, per RFC 2104.
        let long = [0x3fu8; 200];
        assert_eq!(mac(&long, b"m"), mac(&Sha256::digest(long), b"m"));
    }

    #[test]
    fn streaming_updates_match_one_shot() {
        let key = b"a key of moderate size";
        let msg = b"the quick brown fox jumps over the lazy dog, repeatedly and at length";
        let mut m = HmacSha256::new_from_slice(key).expect("any key length");
        for chunk in msg.chunks(7) {
            m.update(chunk);
        }
        assert_eq!(m.finalize(), mac(key, msg));
    }

    // -----------------------------------------------------------------------
    // Verification
    // -----------------------------------------------------------------------

    #[test]
    fn verify_slice_accepts_the_right_tag() {
        let key = b"k";
        let tag = mac(key, b"payload");
        let mut m = HmacSha256::new_from_slice(key).expect("any key length");
        m.update(b"payload");
        assert_eq!(m.verify_slice(&tag), Ok(()));
    }

    #[test]
    fn verify_slice_rejects_a_forged_tag() {
        let key = b"k";
        let mut tag = mac(key, b"payload");

        // Flip the LAST byte -- the case a first-mismatch loop would answer
        // slowest, and the one a timing attacker walks toward one byte at a
        // time.
        tag[31] ^= 0x01;
        let mut m = HmacSha256::new_from_slice(key).expect("any key length");
        m.update(b"payload");
        assert_eq!(m.verify_slice(&tag), Err(MacError));

        // Flip the FIRST byte instead.
        tag[31] ^= 0x01;
        tag[0] ^= 0x80;
        let mut m = HmacSha256::new_from_slice(key).expect("any key length");
        m.update(b"payload");
        assert_eq!(m.verify_slice(&tag), Err(MacError));
    }

    #[test]
    fn verify_slice_rejects_wrong_key_message_and_length() {
        let tag = mac(b"k", b"payload");

        let mut wrong_key = HmacSha256::new_from_slice(b"j").expect("any key length");
        wrong_key.update(b"payload");
        assert!(wrong_key.verify_slice(&tag).is_err());

        let mut wrong_msg = HmacSha256::new_from_slice(b"k").expect("any key length");
        wrong_msg.update(b"payloaD");
        assert!(wrong_msg.verify_slice(&tag).is_err());

        // A truncated tag must not validate on its prefix.
        let mut truncated = HmacSha256::new_from_slice(b"k").expect("any key length");
        truncated.update(b"payload");
        assert!(truncated.verify_slice(&tag[..16]).is_err());

        // Nor an over-long one.
        let mut extended = HmacSha256::new_from_slice(b"k").expect("any key length");
        extended.update(b"payload");
        let mut longer = tag.to_vec();
        longer.push(0);
        assert!(extended.verify_slice(&longer).is_err());
    }

    #[test]
    fn errors_render_without_leaking() {
        assert_eq!(InvalidLength.to_string(), "invalid key length");
        assert_eq!(MacError.to_string(), "MAC tag mismatch");

        let mut m = HmacSha256::new_from_slice(b"a very secret key").expect("any key length");
        m.update(b"x");
        let rendered = format!("{m:?}");
        assert!(!rendered.contains("secret"), "{rendered}");
        assert!(rendered.starts_with("HmacSha256"), "{rendered}");
    }

    #[test]
    fn hex_vectors_decode_as_expected() {
        // Guards the test helper itself: RFC 4231 case 2's key in hex.
        assert_eq!(unhex("4a656665"), b"Jefe".to_vec());
    }
}
