// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0 AND (MIT OR Apache-2.0)
//
// The generated code and the attribute decisions are derived from upstream
// `serde_derive` 1.0.228 (https://github.com/serde-rs/serde), dual-licensed
// MIT OR Apache-2.0; see LICENSE-MIT and LICENSE-APACHE. Each derived file
// names its upstream source. This file's `wrap_in_const` and
// `allow_deprecated` are derived from serde_derive 1.0.228 src/dummy.rs and
// src/deprecated.rs; the rest of it is aterm's.

//! `#[derive(Serialize, Deserialize)]` proc macros for `aterm-serde`.

#![allow(
    clippy::enum_variant_names,
    reason = "upstream serde_derive 1.0.228 allows it crate-wide too: StructTrait and \
              TupleTrait name the serde traits they select, so each file still diffs \
              against upstream"
)]

#[macro_use]
mod fragment;

mod ast;
mod attr;
mod bound;
mod case;
mod closed_set;
mod de;
mod ser;

use aterm_macro_support::{AttrArgs, Attribute, DeriveInput, Error, parse_derive_input, q};
use proc_macro::{Span, TokenStream, TokenTree};

/// Derives `serde::Serialize` (aterm-serde) for a struct or an enum.
#[proc_macro_derive(Serialize, attributes(serde))]
pub fn derive_serialize(input: TokenStream) -> TokenStream {
    expand(input, ser::expand_derive_serialize)
}

/// Derives `serde::Deserialize` (aterm-serde) for a struct or an enum.
#[proc_macro_derive(Deserialize, attributes(serde))]
pub fn derive_deserialize(input: TokenStream) -> TokenStream {
    expand(input, de::expand_derive_deserialize)
}

fn expand(
    input: TokenStream,
    derive: fn(&DeriveInput) -> Result<TokenStream, TokenStream>,
) -> TokenStream {
    let input = match parse_derive_input(input) {
        Ok(input) => input,
        Err(err) => return err.to_compile_error(),
    };
    let input = ast::replace_receiver(&input);
    match derive(&input) {
        Ok(tokens) | Err(tokens) => {
            if tokens.is_empty() {
                Error::new(Span::call_site(), "the serde derive produced nothing")
                    .to_compile_error()
            } else {
                tokens
            }
        }
    }
}

/// The impl inside an unnamed const, with `_serde` naming aterm-serde.
pub(crate) fn wrap_in_const(code: &TokenStream) -> TokenStream {
    q!(
        "#[doc(hidden)]
        #[allow(
            non_upper_case_globals,
            unused_attributes,
            unused_qualifications,
            clippy::absolute_paths,
        )]
        const _: () = {
            use ::serde as _serde;

            #code
        };",
        code
    )
}

/// `#[allow(deprecated)]` when the item or a variant is deprecated or allows deprecated items.
pub(crate) fn allow_deprecated(input: &DeriveInput) -> Option<TokenStream> {
    if should_allow_deprecated(input) {
        Some(q!("#[allow(deprecated)]"))
    } else {
        None
    }
}

/// Determine if an `#[allow(deprecated)]` should be added to the derived impl.
fn should_allow_deprecated(input: &DeriveInput) -> bool {
    if contains_deprecated(&input.attrs) {
        return true;
    }
    if let aterm_macro_support::Data::Enum(variants) = &input.data {
        for variant in variants {
            if contains_deprecated(&variant.attrs) {
                return true;
            }
        }
    }
    false
}

/// Check whether the given attributes contains one of: - `#[deprecated]` - `#[allow(deprecated)]`.
fn contains_deprecated(attrs: &[Attribute]) -> bool {
    for attr in attrs {
        if attr.is("deprecated") {
            return true;
        }
        if attr.is("allow")
            && let AttrArgs::List(list) = &attr.args
            && list.stream().into_iter().any(
                |tt| matches!(tt, TokenTree::Ident(ident) if ident.to_string() == "deprecated"),
            )
        {
            return true;
        }
    }
    false
}
