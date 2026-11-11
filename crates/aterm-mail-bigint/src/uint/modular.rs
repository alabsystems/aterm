// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

mod reduction;

/// Implements `Residue`s, supporting modular arithmetic with a constant modulus.
pub mod constant_mod;
/// Implements `DynResidue`s, supporting modular arithmetic with a modulus set at runtime.
pub mod runtime_mod;

mod add;
mod div_by_2;
mod inv;
mod mul;
mod pow;
mod sub;

pub use reduction::montgomery_reduction;

/// A generalization for numbers kept in optimized representations (e.g. Montgomery)
/// that can be converted back to the original form.
pub trait Retrieve {
    /// The original type.
    type Output;

    /// Convert the number back from the optimized representation.
    fn retrieve(&self) -> Self::Output;
}

#[cfg(test)]
#[path = "modular_tests.rs"]
mod tests;
