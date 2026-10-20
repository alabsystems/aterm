// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! HMAC-SHA256, transcribed from RFC 2104.
//!
//! `HMAC(K, m) = H((K' ^ opad) || H((K' ^ ipad) || m))`, where `K'` is the key
//! zero-padded to the hash's block size, or `H(K)` zero-padded when the key is
//! longer than a block.

use crate::ct_eq;
use astream_digest::{BLOCK_LEN, OUTPUT_LEN, Sha256};

/// Inner padding byte -- RFC 2104 sec. 2.
const IPAD: u8 = 0x36;

/// Outer padding byte -- RFC 2104 sec. 2.
const OPAD: u8 = 0x5c;

/// Returned by [`HmacSha256::new_from_slice`] for API compatibility with the
/// `hmac` crate. Never produced: HMAC accepts a key of any length.
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
/// Carries nothing: reporting how a tag was wrong would be a forgery oracle.
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
/// [`finalize`](HmacSha256::finalize) returns a plain `[u8; 32]`.
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
    /// Keys longer than the 64-byte block are replaced by their SHA-256
    /// digest; shorter keys are zero-padded.
    ///
    /// # Errors
    ///
    /// Never. See [`InvalidLength`].
    pub const fn new_from_slice(key: &[u8]) -> Result<Self, InvalidLength> {
        // K' -- the key reduced to exactly one block.
        let mut padded = [0u8; BLOCK_LEN];
        if key.len() > BLOCK_LEN {
            let hk = Sha256::digest_slice(key);
            let mut i = 0usize;
            while i < OUTPUT_LEN {
                padded[i] = hk[i];
                i += 1;
            }
        } else {
            let mut i = 0usize;
            while i < key.len() && i < BLOCK_LEN {
                padded[i] = key[i];
                i += 1;
            }
        }

        let mut ipad_block = [0u8; BLOCK_LEN];
        let mut outer_key = [0u8; BLOCK_LEN];
        let mut idx = 0usize;
        while idx < BLOCK_LEN {
            let k = padded[idx];
            ipad_block[idx] = k ^ IPAD;
            outer_key[idx] = k ^ OPAD;
            idx += 1;
        }

        let mut inner = Sha256::new();
        inner.update_slice(&ipad_block);
        Ok(Self { inner, outer_key })
    }

    /// Feeds more bytes into the authenticated message.
    pub const fn update(&mut self, bytes: &[u8]) {
        self.inner.update_slice(bytes);
    }

    /// Finishes the MAC and returns the 32-byte tag.
    #[must_use]
    pub const fn finalize(self) -> [u8; OUTPUT_LEN] {
        let inner = self.inner.finalize();
        let mut outer = Sha256::new();
        outer.update_slice(&self.outer_key);
        outer.update_slice(&inner);
        outer.finalize()
    }

    /// Checks `tag` against the computed MAC in constant time (via [`ct_eq`]).
    ///
    /// # Errors
    ///
    /// [`MacError`] when `tag` is not the MAC of the message fed in, including
    /// a truncated tag.
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
    use crate::hex;

    fn mac(key: &[u8], data: &[u8]) -> [u8; OUTPUT_LEN] {
        let mut m = HmacSha256::new_from_slice(key).expect("HMAC-SHA256 accepts any key length");
        m.update(data);
        m.finalize()
    }

    // -----------------------------------------------------------------------
    // RFC 4231 test vectors for HMAC-SHA256
    // -----------------------------------------------------------------------

    /// RFC 4231 §4.2–4.8, one row per test case: `(case, key, data, tag)`.
    /// Case 5 is published truncated to 128 bits, so its tag is compared over
    /// the first 16 bytes only.
    #[test]
    fn rfc4231_test_vectors() {
        let case4_key: Vec<u8> = (0x01u8..=0x19).collect();
        assert_eq!(case4_key.len(), 25);
        let case7_data: &[u8] = b"This is a test using a larger than block-size key and a larger than block-size data. The key needs to be hashed before being used by the HMAC algorithm.";
        assert_eq!(case7_data.len(), 152);
        for (case, key, data, tag) in [
            (
                1,
                &[0x0b; 20][..],
                &b"Hi There"[..],
                "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
            ),
            // A key shorter than the digest, exercising the zero-pad path.
            (
                2,
                b"Jefe",
                b"what do ya want for nothing?",
                "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843",
            ),
            (
                3,
                &[0xaa; 20],
                &[0xdd; 50],
                "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe",
            ),
            (
                4,
                &case4_key,
                &[0xcd; 50],
                "82558a389a443c0ea4cc819899f2083a85f0faa3e578f8077a2e3ff46729665b",
            ),
            // Published truncated to 128 bits.
            (
                5,
                &[0x0c; 20],
                b"Test With Truncation",
                "a3b6167473100ee06e0c796c2955552b",
            ),
            // 131-byte key: longer than the 64-byte block, so it is hashed first.
            (
                6,
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First",
                "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54",
            ),
            // Over-block-size key and over-block-size data.
            (
                7,
                &[0xaa; 131],
                case7_data,
                "9b09ffa71b942fcb27635fbcd5b0e944bfdc63644f0713938a7f51535c3a35e2",
            ),
        ] {
            let full = mac(key, data);
            assert_eq!(hex(&full[..tag.len() / 2]), tag, "RFC 4231 case {case}");
        }
    }

    // -----------------------------------------------------------------------
    // Verification
    // -----------------------------------------------------------------------

    #[test]
    fn verify_slice_rejects_a_truncated_or_extended_tag() {
        let tag = mac(b"k", b"payload");

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
    fn debug_does_not_leak_the_key() {
        let mut m = HmacSha256::new_from_slice(b"a very secret key").expect("any key length");
        m.update(b"x");
        let rendered = format!("{m:?}");
        assert!(!rendered.contains("secret"), "{rendered}");
        assert!(rendered.starts_with("HmacSha256"), "{rendered}");
    }
}
