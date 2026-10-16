// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The items of an attribute list: `#[name(a, b = "x", c(...))]`.

use proc_macro::{Delimiter, Group, Ident, Punct, Span, TokenTree};

use crate::cursor::{Cursor, is_punct};
use crate::error::Error;
use crate::lit::{StrLit, str_lit};

/// One item of an attribute list.
#[derive(Clone, Debug)]
pub struct Meta {
    /// The `::` before the key, when it is written `::name`.
    pub leading_colon: Option<Span>,
    /// The key's path segments: `rename`, or `a::b`.
    pub path: Vec<Ident>,
    /// What follows the key.
    pub kind: MetaKind,
}

/// What follows an item's key.
#[derive(Clone, Debug)]
pub enum MetaKind {
    /// Nothing: `name`.
    Flag,
    /// `name = value`.
    Value {
        /// The `=`.
        eq: Punct,
        /// The value's tokens, up to the next comma.
        value: Vec<TokenTree>,
    },
    /// `name(...)`.
    List(Group),
}

impl Meta {
    /// Whether the key is the single segment `name`, with no `::` before it
    /// (`::name` is another key).
    #[must_use]
    pub fn is(&self, name: &str) -> bool {
        self.leading_colon.is_none() && self.path.len() == 1 && self.path[0].to_string() == name
    }

    /// The key as written, segments joined by `::`, a leading `::` kept.
    #[must_use]
    pub fn key(&self) -> String {
        let joined = self
            .path
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("::");
        if self.leading_colon.is_some() {
            format!("::{joined}")
        } else {
            joined
        }
    }

    /// The span of the key's first token: its leading `::` or first segment.
    #[must_use]
    pub fn first_span(&self) -> Span {
        self.leading_colon
            .or_else(|| self.path.first().map(Ident::span))
            .unwrap_or_else(Span::call_site)
    }

    /// The span of the item's last token.
    #[must_use]
    pub fn last_span(&self) -> Span {
        match &self.kind {
            MetaKind::Flag => self.path.last().map_or_else(Span::call_site, Ident::span),
            MetaKind::Value { eq, value } => {
                value.last().map_or_else(|| eq.span(), TokenTree::span)
            }
            MetaKind::List(group) => group.span(),
        }
    }

    /// An error over the whole item.
    pub fn error(&self, message: impl std::fmt::Display) -> Error {
        Error::spanned(self.first_span(), self.last_span(), message)
    }

    /// An error over the key.
    pub fn key_error(&self, message: impl std::fmt::Display) -> Error {
        let last = self.path.last().map_or_else(Span::call_site, Ident::span);
        Error::spanned(self.first_span(), last, message)
    }
}

/// Parses the items inside `group`, the `(...)` of a list attribute.
///
/// # Errors
/// An item that does not start with a path, or that is followed by anything
/// but `=`, `(...)`, `,` or the end.
pub fn parse_list(group: &Group) -> Result<Vec<Meta>, Error> {
    let mut c = Cursor::of_group(group);
    let mut items = Vec::new();
    while !c.is_empty() {
        let mut path = Vec::new();
        let leading_colon = c.eat_op("::");
        path.push(c.expect_ident("an attribute key")?);
        while c.eat_op("::").is_some() {
            path.push(c.expect_ident("a path segment")?);
        }
        let kind = if let Some(eq) = c.eat_punct('=') {
            let mut value = Vec::new();
            while let Some(tt) = c.peek_raw() {
                if is_punct(tt, ',') {
                    break;
                }
                if let Some(tt) = c.next_raw() {
                    value.push(tt);
                }
            }
            if value.is_empty() {
                return Err(Error::new(eq.span(), "expected a value after `=`"));
            }
            MetaKind::Value { eq, value }
        } else if let Some(g) = c.eat_group(Delimiter::Parenthesis) {
            MetaKind::List(g)
        } else {
            MetaKind::Flag
        };
        items.push(Meta {
            leading_colon,
            path,
            kind,
        });
        if c.eat_punct(',').is_none() && !c.is_empty() {
            return Err(Error::new(c.span(), "expected `,`"));
        }
    }
    Ok(items)
}

/// A value's string literal and its span, None-groups unwrapped (a `$lit`
/// fragment arrives in one). `None` when the value is not one string
/// literal.
#[must_use]
pub fn value_str(value: &[TokenTree]) -> Option<(StrLit, Span)> {
    let mut tokens = value.to_vec();
    loop {
        match tokens.as_slice() {
            [TokenTree::Group(g)] if g.delimiter() == Delimiter::None => {
                tokens = g.stream().into_iter().collect();
            }
            [TokenTree::Literal(lit)] => return str_lit(lit).map(|s| (s, lit.span())),
            _ => return None,
        }
    }
}
