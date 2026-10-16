// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde_derive 1.0.228 src/fragment.rs and modified by the aterm
// project in 2026 (std proc_macro tokens instead of proc-macro2/quote).
// See ../LICENSE-MIT and ../LICENSE-APACHE for the upstream licenses.

use aterm_macro_support::{ToTokens, braced};
use proc_macro::{Punct, Spacing, TokenStream, TokenTree};

pub enum Fragment {
    /// Tokens that can be used as an expression.
    Expr(TokenStream),
    /// Tokens that can be used inside a block. The surrounding curly braces are
    /// not part of these tokens.
    Block(TokenStream),
}

macro_rules! quote_expr {
    ($($tt:tt)*) => {
        $crate::fragment::Fragment::Expr(aterm_macro_support::q!($($tt)*))
    }
}

macro_rules! quote_block {
    ($($tt:tt)*) => {
        $crate::fragment::Fragment::Block(aterm_macro_support::q!($($tt)*))
    }
}

/// Interpolate a fragment in place of an expression. This involves surrounding
/// Block fragments in curly braces.
pub struct Expr(pub Fragment);
impl ToTokens for Expr {
    fn to_tokens(&self, out: &mut Vec<TokenTree>) {
        match &self.0 {
            Fragment::Expr(expr) => expr.to_tokens(out),
            Fragment::Block(block) => out.push(braced(block.clone())),
        }
    }
}

/// Interpolate a fragment as the statements of a block.
pub struct Stmts(pub Fragment);
impl ToTokens for Stmts {
    fn to_tokens(&self, out: &mut Vec<TokenTree>) {
        match &self.0 {
            Fragment::Expr(expr) => expr.to_tokens(out),
            Fragment::Block(block) => block.to_tokens(out),
        }
    }
}

/// Interpolate a fragment as the value part of a `match` expression. This
/// involves putting a comma after expressions and curly braces around blocks.
pub struct Match(pub Fragment);
impl ToTokens for Match {
    fn to_tokens(&self, out: &mut Vec<TokenTree>) {
        match &self.0 {
            Fragment::Expr(expr) => {
                expr.to_tokens(out);
                out.push(TokenTree::Punct(Punct::new(',', Spacing::Alone)));
            }
            Fragment::Block(block) => out.push(braced(block.clone())),
        }
    }
}
