// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! First-party `document-features` stub patched in for `wgpu` and `wgpu-core`.
//!
//! [`document_features!`] expands to `""` regardless of arguments; the `self-test`
//! feature is declared and inert.

use proc_macro::{Literal, TokenStream, TokenTree};

/// Expands to `""` regardless of arguments.
#[proc_macro]
pub fn document_features(_args: TokenStream) -> TokenStream {
    TokenTree::Literal(Literal::string("")).into()
}
