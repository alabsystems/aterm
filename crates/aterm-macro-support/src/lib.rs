// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A syn-free toolkit for aterm's proc macros, on std `proc_macro` alone
//! (docs/AUDIT-third-party-2026-10-15.md, SP.5).
//!
//! - [`Cursor`]: reads one level of a token stream front to back, splicing
//!   None-delimited groups (what a `macro_rules!` fragment arrives as) into the
//!   stream where the grammar is structural, and keeping them whole where a
//!   type is taken as an opaque run.
//! - [`DeriveInput`]: the item a derive receives: attributes, visibility,
//!   generics with bounds, defaults and where clauses, and the struct, enum or
//!   union body. Field types stay token runs, emitted as written.
//! - [`Ty`]: a reading of a type for analysis (which type parameters a field
//!   mentions, which lifetimes it could borrow). Anything it does not
//!   understand reads as [`Ty::Other`]; the compiler judges the type itself.
//! - [`lit`]: string literals, raw and escaped, integer literals in any base,
//!   and the paths serde writes as strings.
//! - [`meta`]: `name`, `name = value` and `name(...)` lists inside an
//!   attribute.
//! - [`quote()`]: string templates with `#name` holes, the generated code's
//!   spelling. Template tokens take the call site's span (or one given span);
//!   spliced tokens keep their own, so a diagnostic still points into the
//!   user's source.
//! - [`Error`]/[`Errors`]: `compile_error!` at a span, never a panic. Every
//!   parser here returns an error on input it does not accept.

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
