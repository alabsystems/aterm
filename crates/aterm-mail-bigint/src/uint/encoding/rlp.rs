// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Recursive Length Prefix (RLP) encoding support.

use crate::{Encoding, Uint};
use rlp::{DecoderError, Rlp, RlpStream};

impl<const LIMBS: usize> rlp::Encodable for Uint<LIMBS>
where
    Self: Encoding,
{
    fn rlp_append(&self, stream: &mut RlpStream) {
        let bytes = self.to_be_bytes();
        let mut bytes_stripped = bytes.as_ref();

        while bytes_stripped.first().cloned() == Some(0) {
            bytes_stripped = &bytes_stripped[1..];
        }

        stream.encoder().encode_value(bytes_stripped);
    }
}

impl<const LIMBS: usize> rlp::Decodable for Uint<LIMBS>
where
    Self: Encoding,
    <Self as Encoding>::Repr: Default,
{
    fn decode(rlp: &Rlp<'_>) -> Result<Self, DecoderError> {
        rlp.decoder().decode_value(|bytes| {
            if bytes.first().cloned() == Some(0) {
                Err(rlp::DecoderError::RlpInvalidIndirection)
            } else {
                let mut repr = <Self as Encoding>::Repr::default();
                let offset = repr
                    .as_ref()
                    .len()
                    .checked_sub(bytes.len())
                    .ok_or(DecoderError::RlpIsTooBig)?;

                repr.as_mut()[offset..].copy_from_slice(bytes);
                Ok(Self::from_be_bytes(repr))
            }
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
#[path = "rlp_tests.rs"]
mod tests;
