// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The item an attribute macro sits on, read only as far as the anchors need.

use aterm_macro_support::{Cursor, is_keyword, is_punct};
use proc_macro::{Delimiter, Group, Ident, Span, TokenStream, TokenTree};

/// The annotated item's tokens, as written.
pub(crate) struct FnItem {
    tokens: Vec<TokenTree>,
}

impl FnItem {
    pub(crate) fn new(item: TokenStream) -> Self {
        FnItem {
            tokens: item.into_iter().collect(),
        }
    }

    /// The item as written.
    pub(crate) fn stream(&self) -> TokenStream {
        self.tokens.iter().cloned().collect()
    }

    /// The top-level tokens with None-delimited groups read through, for
    /// looking at; never emitted.
    fn flat(&self) -> Vec<TokenTree> {
        Cursor::new(self.stream(), Span::call_site()).collect()
    }

    /// The `<name>` of the first top-level `fn <name>`.
    pub(crate) fn ident(&self) -> Option<Ident> {
        let flat = self.flat();
        flat.windows(2).find_map(|pair| match pair {
            [fn_token, TokenTree::Ident(name)] if is_keyword(fn_token, "fn") => Some(name.clone()),
            _ => None,
        })
    }

    /// `Some(is_const)` when the item is a fn with a body.
    pub(crate) fn fn_with_body(&self) -> Option<bool> {
        let flat = self.flat();
        let mut i = 0;
        while flat.get(i).is_some_and(|t| is_punct(t, '#'))
            && matches!(flat.get(i + 1), Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Bracket)
        {
            i += 2;
        }
        if flat.get(i).is_some_and(|t| is_keyword(t, "pub")) {
            i += 1;
            if matches!(flat.get(i), Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Parenthesis)
            {
                i += 1;
            }
        }
        let mut constness = false;
        loop {
            match flat.get(i) {
                Some(t) if is_keyword(t, "const") => {
                    constness = true;
                    i += 1;
                }
                Some(t)
                    if ["default", "async", "unsafe", "safe"]
                        .iter()
                        .any(|w| is_keyword(t, w)) =>
                {
                    i += 1;
                }
                Some(t) if is_keyword(t, "extern") => {
                    i += 1;
                    if matches!(flat.get(i), Some(TokenTree::Literal(_))) {
                        i += 1;
                    }
                }
                _ => break,
            }
        }
        let named_fn = flat.get(i).is_some_and(|t| is_keyword(t, "fn"))
            && matches!(flat.get(i + 1), Some(TokenTree::Ident(_)));
        let body_last =
            matches!(flat.last(), Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Brace);
        (named_fn && body_last).then_some(constness)
    }

    /// The item with `probe` as the first statement of its body, after the body's inner attributes.
    pub(crate) fn with_probe(&self, probe: &TokenStream) -> Option<TokenStream> {
        if self.fn_with_body()? {
            return None;
        }
        let (last, rest) = self.tokens.split_last()?;
        let body = prepend(last, probe)?;
        Some(rest.iter().cloned().chain(std::iter::once(body)).collect())
    }
}

/// `body` (a `{ ... }` block, or a None-delimited group a `$b:block` fragment
/// wrapped it in) with `probe` after its inner attributes.
fn prepend(body: &TokenTree, probe: &TokenStream) -> Option<TokenTree> {
    let TokenTree::Group(group) = body else {
        return None;
    };
    let inner: Vec<TokenTree> = group.stream().into_iter().collect();
    let stream: TokenStream = match group.delimiter() {
        Delimiter::Brace => {
            let mut attrs = 0;
            while inner.get(attrs).is_some_and(|t| is_punct(t, '#'))
                && inner.get(attrs + 1).is_some_and(|t| is_punct(t, '!'))
                && matches!(inner.get(attrs + 2), Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Bracket)
            {
                attrs += 3;
            }
            let (head, tail) = inner.split_at(attrs);
            head.iter()
                .cloned()
                .chain(probe.clone())
                .chain(tail.iter().cloned())
                .collect()
        }
        Delimiter::None => {
            let (last, rest) = inner.split_last()?;
            let body = prepend(last, probe)?;
            rest.iter().cloned().chain(std::iter::once(body)).collect()
        }
        _ => return None,
    };
    let mut rebuilt = Group::new(group.delimiter(), stream);
    rebuilt.set_span(group.span());
    Some(TokenTree::Group(rebuilt))
}
