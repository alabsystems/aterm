// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Syn-free proc-macro toolkit on std `proc_macro` ([`Cursor`], [`DeriveInput`], [`Ty`],
//! [`Ty::Other`], [`lit`], [`meta`], [`quote()`], [`Error`], [`Errors`]).

extern crate proc_macro;

mod cursor;
mod error;
mod item;
pub mod lit;
pub mod meta;
mod quote;
mod ty;

pub use crate::cursor::{Cursor, is_keyword, is_punct, split_commas, split_top};
pub use crate::error::{Error, Errors};
pub use crate::item::{
    AttrArgs, Attribute, Data, DeriveInput, Field, Fields, GenericParam, Generics, Lifetime,
    Variant, parse_derive_input,
};
pub use crate::quote::{ToTokens, braced, join, quote, quote_spanned, respan};
pub use crate::ty::{Bound, GenericArg, PathArgs, Segment, Ty, TyPath, parse_type};
