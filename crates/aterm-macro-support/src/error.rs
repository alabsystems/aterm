// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Errors a macro reports as `compile_error!` at a span.

use std::cell::RefCell;
use std::fmt::Display;

use proc_macro::{Delimiter, Group, Ident, Literal, Punct, Spacing, Span, TokenStream, TokenTree};

/// One diagnostic: a message over the source from `start` to `end`.
///
/// Stable `proc_macro` cannot join two spans, so, as syn does, the emitted
/// `compile_error!` puts its path at `start` and its message at `end`, and
/// the compiler underlines from one to the other.
#[derive(Debug)]
pub struct Error {
    start: Span,
    end: Span,
    message: String,
}

impl Error {
    /// An error at one span.
    pub fn new(span: Span, message: impl Display) -> Self {
        Error {
            start: span,
            end: span,
            message: message.to_string(),
        }
    }

    /// An error from `start` to `end`.
    pub fn spanned(start: Span, end: Span, message: impl Display) -> Self {
        Error {
            start,
            end,
            message: message.to_string(),
        }
    }

    /// An error over `tokens`, first to last; at the call site when empty.
    pub fn spanned_by(tokens: &[TokenTree], message: impl Display) -> Self {
        let start = tokens.first().map_or_else(Span::call_site, TokenTree::span);
        let end = tokens.last().map_or(start, TokenTree::span);
        Error::spanned(start, end, message)
    }

    /// The message, as the compiler will print it.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// `::core::compile_error! { "message" }`, spanned as described on
    /// [`Error`].
    #[must_use]
    pub fn to_compile_error(&self) -> TokenStream {
        let punct = |ch: char, spacing: Spacing| {
            let mut p = Punct::new(ch, spacing);
            p.set_span(self.start);
            TokenTree::Punct(p)
        };
        let mut message = Literal::string(&self.message);
        message.set_span(self.end);
        let mut group = Group::new(
            Delimiter::Brace,
            TokenStream::from(TokenTree::Literal(message)),
        );
        group.set_span(self.end);
        [
            punct(':', Spacing::Joint),
            punct(':', Spacing::Alone),
            TokenTree::Ident(Ident::new("core", self.start)),
            punct(':', Spacing::Joint),
            punct(':', Spacing::Alone),
            TokenTree::Ident(Ident::new("compile_error", self.start)),
            punct('!', Spacing::Alone),
            TokenTree::Group(group),
        ]
        .into_iter()
        .collect()
    }
}

/// Collects every error one expansion finds, so the user sees all of them at
/// once rather than the first.
#[derive(Default)]
pub struct Errors {
    list: RefCell<Vec<Error>>,
}

impl Errors {
    /// An empty collection.
    #[must_use]
    pub fn new() -> Self {
        Errors::default()
    }

    /// Records `error`.
    pub fn push(&self, error: Error) {
        self.list.borrow_mut().push(error);
    }

    /// Records an error at `span`.
    pub fn at(&self, span: Span, message: impl Display) {
        self.push(Error::new(span, message));
    }

    /// Records an error from `start` to `end`.
    pub fn spanned(&self, start: Span, end: Span, message: impl Display) {
        self.push(Error::spanned(start, end, message));
    }

    /// Whether nothing has been recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.list.borrow().is_empty()
    }

    /// `Ok` when nothing was recorded, else every error as `compile_error!`s.
    ///
    /// # Errors
    /// At least one error was recorded.
    pub fn finish(self) -> Result<(), TokenStream> {
        let list = self.list.into_inner();
        if list.is_empty() {
            Ok(())
        } else {
            Err(list.iter().map(Error::to_compile_error).collect())
        }
    }
}
