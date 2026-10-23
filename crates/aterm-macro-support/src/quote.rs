// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! String templates with `#name` holes for generated code.

use proc_macro::{Delimiter, Group, Ident, Literal, Punct, Span, TokenStream, TokenTree};

use crate::error::Error;

/// Something that writes itself as tokens.
pub trait ToTokens {
    /// Appends this value's tokens to `out`.
    fn to_tokens(&self, out: &mut Vec<TokenTree>);

    /// This value's tokens as a stream.
    fn to_token_stream(&self) -> TokenStream {
        let mut out = Vec::new();
        self.to_tokens(&mut out);
        out.into_iter().collect()
    }
}

impl ToTokens for TokenStream {
    #[inline]
    fn to_tokens(&self, out: &mut Vec<TokenTree>) {
        out.extend(self.clone());
    }
}

impl ToTokens for TokenTree {
    #[inline]
    fn to_tokens(&self, out: &mut Vec<TokenTree>) {
        out.push(self.clone());
    }
}

impl ToTokens for Ident {
    #[inline]
    fn to_tokens(&self, out: &mut Vec<TokenTree>) {
        out.push(TokenTree::Ident(self.clone()));
    }
}

impl ToTokens for Literal {
    #[inline]
    fn to_tokens(&self, out: &mut Vec<TokenTree>) {
        out.push(TokenTree::Literal(self.clone()));
    }
}

impl ToTokens for Punct {
    #[inline]
    fn to_tokens(&self, out: &mut Vec<TokenTree>) {
        out.push(TokenTree::Punct(self.clone()));
    }
}

impl ToTokens for Group {
    #[inline]
    fn to_tokens(&self, out: &mut Vec<TokenTree>) {
        out.push(TokenTree::Group(self.clone()));
    }
}

impl<T: ToTokens + ?Sized> ToTokens for &T {
    #[inline]
    fn to_tokens(&self, out: &mut Vec<TokenTree>) {
        (**self).to_tokens(out);
    }
}

impl<T: ToTokens> ToTokens for Option<T> {
    #[inline]
    fn to_tokens(&self, out: &mut Vec<TokenTree>) {
        if let Some(value) = self {
            value.to_tokens(out);
        }
    }
}

impl<T: ToTokens> ToTokens for [T] {
    #[inline]
    fn to_tokens(&self, out: &mut Vec<TokenTree>) {
        for value in self {
            value.to_tokens(out);
        }
    }
}

impl<T: ToTokens> ToTokens for Vec<T> {
    #[inline]
    fn to_tokens(&self, out: &mut Vec<TokenTree>) {
        self.as_slice().to_tokens(out);
    }
}

impl ToTokens for bool {
    #[inline]
    fn to_tokens(&self, out: &mut Vec<TokenTree>) {
        let word = if *self { "true" } else { "false" };
        out.push(TokenTree::Ident(Ident::new(word, Span::call_site())));
    }
}

/// Joins `items` with `separator`, plus a trailing separator when `trailing` is true.
#[must_use]
pub fn join<T: ToTokens>(items: &[T], separator: char, trailing: bool) -> TokenStream {
    let mut out = Vec::new();
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push(TokenTree::Punct(Punct::new(
                separator,
                proc_macro::Spacing::Alone,
            )));
        }
        item.to_tokens(&mut out);
    }
    if trailing && !items.is_empty() {
        out.push(TokenTree::Punct(Punct::new(
            separator,
            proc_macro::Spacing::Alone,
        )));
    }
    out.into_iter().collect()
}

/// Lexes `template` and fills its `#name` holes from `vars`.
#[inline]
#[must_use]
pub fn quote(template: &str, vars: &[(&str, &dyn ToTokens)]) -> TokenStream {
    expand(template, None, vars)
}

/// [`quote`], with every template token at `span` (the holes keep theirs).
#[inline]
#[must_use]
pub fn quote_spanned(span: Span, template: &str, vars: &[(&str, &dyn ToTokens)]) -> TokenStream {
    expand(template, Some(span), vars)
}

fn expand(template: &str, span: Option<Span>, vars: &[(&str, &dyn ToTokens)]) -> TokenStream {
    let Ok(stream) = template.parse::<TokenStream>() else {
        return internal(template, "does not lex");
    };
    let stream = match span {
        Some(span) => respan(stream, span),
        None => stream,
    };
    let mut out = Vec::new();
    match fill(stream, vars, &mut out) {
        Ok(()) => out.into_iter().collect(),
        Err(hole) => internal(template, &format!("has no binding for #{hole}")),
    }
}

fn internal(template: &str, what: &str) -> TokenStream {
    Error::new(
        Span::call_site(),
        format!("internal error in a derive template: `{template}` {what}"),
    )
    .to_compile_error()
}

fn fill(
    stream: TokenStream,
    vars: &[(&str, &dyn ToTokens)],
    out: &mut Vec<TokenTree>,
) -> Result<(), String> {
    let mut iter = stream.into_iter().peekable();
    while let Some(tt) = iter.next() {
        match tt {
            TokenTree::Punct(p) if p.as_char() == '#' => {
                if let Some(TokenTree::Ident(name)) = iter.peek() {
                    let name = name.to_string();
                    match vars.iter().find(|(var, _)| *var == name) {
                        Some((_, value)) => {
                            iter.next();
                            value.to_tokens(out);
                        }
                        None => return Err(name),
                    }
                } else {
                    out.push(TokenTree::Punct(p));
                }
            }
            TokenTree::Group(g) => {
                let mut inner = Vec::new();
                fill(g.stream(), vars, &mut inner)?;
                let mut filled = Group::new(g.delimiter(), inner.into_iter().collect());
                filled.set_span(g.span());
                out.push(TokenTree::Group(filled));
            }
            other => out.push(other),
        }
    }
    Ok(())
}

/// `stream` with every token, at every depth, at `span`.
#[must_use]
pub fn respan(stream: TokenStream, span: Span) -> TokenStream {
    stream
        .into_iter()
        .map(|tt| match tt {
            TokenTree::Group(g) => {
                let mut group = Group::new(g.delimiter(), respan(g.stream(), span));
                group.set_span(span);
                TokenTree::Group(group)
            }
            TokenTree::Ident(mut i) => {
                i.set_span(span);
                TokenTree::Ident(i)
            }
            TokenTree::Punct(mut p) => {
                p.set_span(span);
                TokenTree::Punct(p)
            }
            TokenTree::Literal(mut l) => {
                l.set_span(span);
                TokenTree::Literal(l)
            }
        })
        .collect()
}

/// A `{ ... }` group around `inner`, at the call site.
#[inline]
#[must_use]
pub fn braced(inner: TokenStream) -> TokenTree {
    TokenTree::Group(Group::new(Delimiter::Brace, inner))
}

/// [`quote()`] with its holes bound by name: `q!("let #x = #y;", x, y = expr)`.
#[macro_export]
macro_rules! q {
    (@arg $name:ident) => { &$name as &dyn $crate::ToTokens };
    (@arg $name:ident = $value:expr) => { &$value as &dyn $crate::ToTokens };
    ($template:literal $(, $name:ident $(= $value:expr)?)* $(,)?) => {
        $crate::quote($template, &[$((stringify!($name), $crate::q!(@arg $name $(= $value)?))),*])
    };
}

/// [`quote_spanned`] with [`q!`]'s bindings: `q_spanned!(span => "...", x)`.
#[macro_export]
macro_rules! q_spanned {
    ($span:expr => $template:literal $(, $name:ident $(= $value:expr)?)* $(,)?) => {
        $crate::quote_spanned($span, $template, &[$((stringify!($name), $crate::q!(@arg $name $(= $value)?))),*])
    };
}
