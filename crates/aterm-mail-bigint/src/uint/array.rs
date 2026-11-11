// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `generic-array` integration with `Uint`.
// TODO(tarcieri): completely phase out `generic-array` when const generics are powerful enough

use crate::{ArrayDecoding, ArrayEncoding, ByteArray};
use generic_array::{GenericArray, typenum};

macro_rules! impl_uint_array_encoding {
    ($(($uint:ident, $bytes:path)),+) => {
        $(
            impl ArrayEncoding for super::$uint {
                type ByteSize = $bytes;

                #[inline]
                fn from_be_byte_array(bytes: ByteArray<Self>) -> Self {
                    Self::from_be_slice(&bytes)
                }

                #[inline]
                fn from_le_byte_array(bytes: ByteArray<Self>) -> Self {
                    Self::from_le_slice(&bytes)
                }

                #[inline]
                fn to_be_byte_array(&self) -> ByteArray<Self> {
                    let mut result = GenericArray::default();
                    self.write_be_bytes(&mut result);
                    result
                }

                #[inline]
                fn to_le_byte_array(&self) -> ByteArray<Self> {
                    let mut result = GenericArray::default();
                    self.write_le_bytes(&mut result);
                    result
                }
            }

            impl ArrayDecoding for GenericArray<u8, $bytes> {
                type Output = super::$uint;

                fn into_uint_be(self) -> Self::Output {
                    Self::Output::from_be_byte_array(self)
                }

                fn into_uint_le(self) -> Self::Output {
                    Self::Output::from_le_byte_array(self)
                }
            }
        )+
     };
}

// TODO(tarcieri): use `generic_const_exprs` when stable to make generic around bits.
impl_uint_array_encoding! {
    (U64, typenum::U8),
    (U128, typenum::U16),
    (U192, typenum::U24),
    (U256, typenum::U32),
    (U384, typenum::U48),
    (U448, typenum::U56),
    (U512, typenum::U64),
    (U576, typenum::U72),
    (U768, typenum::U96),
    (U832, typenum::U104),
    (U896, typenum::U112),
    (U1024, typenum::U128),
    (U1536, typenum::U192),
    (U1792, typenum::U224),
    (U2048, typenum::U256),
    (U3072, typenum::U384),
    (U3584, typenum::U448),
    (U4096, typenum::U512),
    (U6144, typenum::U768),
    (U8192, typenum::U1024)
}

#[cfg(target_pointer_width = "32")]
impl_uint_array_encoding! {
    (U224, typenum::U28), // For NIST P-224
    (U544, typenum::U68)  // For NIST P-521
}

#[cfg(test)]
#[path = "array_tests.rs"]
mod tests;
