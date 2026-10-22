// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A cursor over one level of a token stream.

use proc_macro::{Delimiter, Group, Ident, Punct, Spacing, Span, TokenStream, TokenTree};

use crate::error::Error;

/// Reads one level of a token stream front to back.
///
/// The structural reads ([`Cursor::peek`], [`Cursor::next`] and everything
/// built on them) first splice a None-delimited group at the front into the
/// stream: `$vis:vis`, `$lit:literal` and `$e:expr` fragments arrive wrapped in
/// one, and the grammar around them reads through it. The raw reads keep such a
/// group whole, which is how a `$t:ty` field type stays one type.
pub struct Cursor {
    /// The unread tokens, last first: the next token is `rev.last()`.
    rev: Vec<TokenTree>,
    /// Where "unexpected end of input" points: the group's closing delimiter,
    /// or the call site at the top level.
    end: Span,
}

impl Cursor {
    /// A cursor over `stream`; `end` is where an error at its end points.
    #[must_use]
    pub fn new(stream: TokenStream, end: Span) -> Self {
        let mut rev: Vec<TokenTree> = stream.into_iter().collect();
        rev.reverse();
        Cursor { rev, end }
    }

    /// A cursor over the inside of `group`.
    #[inline]
    #[must_use]
    pub fn of_group(group: &Group) -> Self {
        Cursor::new(group.stream(), group.span_close())
    }

    /// Splices every None-delimited group at the front into the stream.
    pub fn flatten(&mut self) {
        while let Some(TokenTree::Group(group)) = self.rev.last() {
            if group.delimiter() != Delimiter::None {
                break;
            }
            let inner: Vec<TokenTree> = group.stream().into_iter().collect();
            self.rev.pop();
            self.rev.extend(inner.into_iter().rev());
        }
    }

    /// The next token, None-delimited groups included as written.
    #[inline]
    #[must_use]
    pub fn peek_raw(&self) -> Option<&TokenTree> {
        self.rev.last()
    }

    /// The token `n` places ahead (0 is the next), as written.
    #[inline]
    #[must_use]
    pub fn peek_raw_nth(&self, n: usize) -> Option<&TokenTree> {
        self.rev
            .len()
            .checked_sub(n + 1)
            .and_then(|i| self.rev.get(i))
    }

    /// Takes the next token as written.
    #[inline]
    pub fn next_raw(&mut self) -> Option<TokenTree> {
        self.rev.pop()
    }

    /// The next token, after splicing None-delimited groups.
    #[inline]
    pub fn peek(&mut self) -> Option<&TokenTree> {
        self.flatten();
        self.rev.last()
    }

    /// Whether every token has been read (an empty None-group reads as
    /// nothing).
    #[inline]
    pub fn is_empty(&mut self) -> bool {
        self.flatten();
        self.rev.is_empty()
    }

    /// The span of the next token, or the end span when there is none.
    #[inline]
    pub fn span(&mut self) -> Span {
        match self.peek() {
            Some(tt) => tt.span(),
            None => self.end,
        }
    }

    /// The span "unexpected end of input" points at.
    #[inline]
    #[must_use]
    pub const fn end_span(&self) -> Span {
        self.end
    }

    /// Whether the next token is the punctuation `ch`.
    #[inline]
    pub fn peek_punct(&mut self, ch: char) -> bool {
        matches!(self.peek(), Some(TokenTree::Punct(p)) if p.as_char() == ch)
    }

    /// Takes the next token if it is the punctuation `ch`.
    #[inline]
    pub fn eat_punct(&mut self, ch: char) -> Option<Punct> {
        if self.peek_punct(ch) {
            match self.rev.pop() {
                Some(TokenTree::Punct(p)) => Some(p),
                _ => None,
            }
        } else {
            None
        }
    }

    /// Whether the next two tokens spell the two-character operator `op`
    /// (`::`, `->`, `=>`): the first joint to the second.
    pub fn peek_op(&mut self, op: &str) -> bool {
        self.flatten();
        let mut chars = op.chars();
        let (Some(a), Some(b), None) = (chars.next(), chars.next(), chars.next()) else {
            return false;
        };
        let first = matches!(self.peek_raw_nth(0),
            Some(TokenTree::Punct(p)) if p.as_char() == a && p.spacing() == Spacing::Joint);
        let second = matches!(self.peek_raw_nth(1),
            Some(TokenTree::Punct(p)) if p.as_char() == b);
        first && second
    }

    /// Takes the two-character operator `op` if it is next; returns the span
    /// of its first character.
    pub fn eat_op(&mut self, op: &str) -> Option<Span> {
        if self.peek_op(op) {
            let first = self.rev.pop().map(|tt| tt.span());
            self.rev.pop();
            first
        } else {
            None
        }
    }

    /// Whether the next token is the identifier `word` (a raw `r#word` is not).
    #[inline]
    pub fn peek_keyword(&mut self, word: &str) -> bool {
        matches!(self.peek(), Some(TokenTree::Ident(i)) if i.to_string() == word)
    }

    /// Takes the next token if it is the identifier `word`.
    #[inline]
    pub fn eat_keyword(&mut self, word: &str) -> Option<Ident> {
        if self.peek_keyword(word) {
            match self.rev.pop() {
                Some(TokenTree::Ident(i)) => Some(i),
                _ => None,
            }
        } else {
            None
        }
    }

    /// Takes the next token if it is a group with this delimiter.
    pub fn eat_group(&mut self, delimiter: Delimiter) -> Option<Group> {
        match self.peek() {
            Some(TokenTree::Group(g)) if g.delimiter() == delimiter => match self.rev.pop() {
                Some(TokenTree::Group(g)) => Some(g),
                _ => None,
            },
            _ => None,
        }
    }

    /// Takes an identifier, or says `expected {what}` at the next token.
    ///
    /// # Errors
    /// The next token is not an identifier, or there is none.
    pub fn expect_ident(&mut self, what: &str) -> Result<Ident, Error> {
        match self.peek() {
            Some(TokenTree::Ident(_)) => match self.rev.pop() {
                Some(TokenTree::Ident(i)) => Ok(i),
                _ => Err(Error::new(self.end, format!("expected {what}"))),
            },
            _ => Err(Error::new(self.span(), format!("expected {what}"))),
        }
    }

    /// Takes the punctuation `ch`, or says `expected {what}`.
    ///
    /// # Errors
    /// The next token is anything else.
    pub fn expect_punct(&mut self, ch: char, what: &str) -> Result<Punct, Error> {
        match self.eat_punct(ch) {
            Some(p) => Ok(p),
            None => Err(Error::new(self.span(), format!("expected {what}"))),
        }
    }

    /// Takes the tokens up to, not including, the first one `stop` accepts at
    /// the top level, keeping None-delimited groups whole.
    ///
    /// Top level means outside every `<...>` the run opens itself (a bracketed
    /// group is already one token). `->` closes no angle, and a `>` with no
    /// open angle is handed to `stop` like any other token. `stop` sees the
    /// token and whether it is at the top level.
    ///
    /// Every `<` opens an angle, which is right for a type, a bound or a where
    /// clause and wrong for an expression, where `<` and `<<` compare and
    /// shift: read one with [`Cursor::take_expr`].
    pub fn take_run(&mut self, stop: impl Fn(&TokenTree, bool) -> bool) -> Vec<TokenTree> {
        let mut out = Vec::new();
        let mut depth = 0usize;
        let mut after_joint_minus = false;
        while let Some(tt) = self.rev.last() {
            let arrow_head = after_joint_minus && is_punct(tt, '>');
            if !arrow_head && stop(tt, depth == 0) {
                break;
            }
            let mut joint_minus = false;
            if let TokenTree::Punct(p) = tt {
                match p.as_char() {
                    '<' => depth += 1,
                    '>' if !arrow_head => depth = depth.saturating_sub(1),
                    '-' => joint_minus = p.spacing() == Spacing::Joint,
                    _ => {}
                }
            }
            after_joint_minus = joint_minus;
            if let Some(tt) = self.rev.pop() {
                out.push(tt);
            }
        }
        out
    }

    /// Takes an expression: the tokens up to, not including, the first comma
    /// outside every generic-argument list the expression opens, keeping
    /// None-delimited groups whole.
    ///
    /// An expression's `<` compares or shifts unless it opens generic
    /// arguments, which it does after `::` (a turbofish), where an operand is
    /// due (a qualified path, `<T as Trait>::C`), and in the type an `as` cast
    /// or a `->` names (`x as u8 < 3` is the compiler's error too). Inside a
    /// list every `<` and `>` counts, as in [`Cursor::take_run`]. So
    /// `1 << 0, B` stops at the comma and `f::<A, B>() < 2, C` after the `2`.
    ///
    /// # Errors
    /// A generic-argument list still open at the end of the input: the comma
    /// that ends the expression is not to be found, and the tokens after it
    /// must not be swallowed into it.
    pub fn take_expr(&mut self) -> Result<Vec<TokenTree>, Error> {
        let mut out = Vec::new();
        let mut due = Due::Operand;
        let mut depth = 0usize;
        let mut opened = self.end;
        // The punctuation just taken, when it is joined to this token.
        let mut joined: Option<char> = None;
        let mut label = false;
        while let Some(tt) = self.rev.last() {
            if depth > 0 {
                if let TokenTree::Punct(p) = tt {
                    match p.as_char() {
                        '<' => depth += 1,
                        '>' if joined != Some('-') => {
                            depth -= 1;
                            if depth == 0 {
                                due = Due::Operator;
                            }
                        }
                        _ => {}
                    }
                }
            } else {
                match tt {
                    TokenTree::Punct(p) if p.as_char() == ',' => break,
                    TokenTree::Punct(p) => {
                        let ty = matches!(due, Due::TypeStart | Due::TypePath);
                        due = match (p.as_char(), joined) {
                            // The second `<` of a `<<` shift.
                            ('<', Some('<')) => Due::Operand,
                            ('<', Some(':')) => {
                                depth = 1;
                                opened = p.span();
                                Due::Operator
                            }
                            ('<', _) if due != Due::Operator => {
                                depth = 1;
                                opened = p.span();
                                Due::Operator
                            }
                            ('>', Some('-')) => Due::TypeStart,
                            (':', _) if ty => Due::TypePath,
                            ('&' | '*', _) if due == Due::TypeStart => Due::TypeStart,
                            ('\'', _) => {
                                label = true;
                                due
                            }
                            ('?', _) => Due::Operator,
                            _ => Due::Operand,
                        };
                    }
                    TokenTree::Ident(i) => {
                        let word = i.to_string();
                        let ty = matches!(due, Due::TypeStart | Due::TypePath);
                        due = if std::mem::take(&mut label) {
                            // A lifetime in a type, or a label before `:` or
                            // after `break`.
                            if ty { Due::TypeStart } else { Due::Operand }
                        } else if word == "as" {
                            Due::TypeStart
                        } else if ty {
                            if matches!(word.as_str(), "mut" | "const" | "dyn" | "impl") {
                                Due::TypeStart
                            } else {
                                Due::TypePath
                            }
                        } else if TAKES_OPERAND.contains(&word.as_str()) {
                            Due::Operand
                        } else {
                            Due::Operator
                        };
                    }
                    TokenTree::Literal(_) | TokenTree::Group(_) => due = Due::Operator,
                }
            }
            joined = match tt {
                TokenTree::Punct(p) if p.spacing() == Spacing::Joint => Some(p.as_char()),
                _ => None,
            };
            if let Some(tt) = self.rev.pop() {
                out.push(tt);
            }
        }
        if depth > 0 {
            return Err(Error::new(
                opened,
                "this `<` opens generic arguments that do not close before the end",
            ));
        }
        Ok(out)
    }

    /// Takes everything left, as written.
    #[inline]
    #[must_use]
    pub fn rest(mut self) -> Vec<TokenTree> {
        self.rev.reverse();
        self.rev
    }
}

/// Takes the next token, after splicing None-delimited groups.
impl Iterator for Cursor {
    type Item = TokenTree;

    #[inline]
    fn next(&mut self) -> Option<TokenTree> {
        self.flatten();
        self.rev.pop()
    }
}

/// Whether `tt` is the punctuation `ch`.
#[inline]
#[must_use]
pub fn is_punct(tt: &TokenTree, ch: char) -> bool {
    matches!(tt, TokenTree::Punct(p) if p.as_char() == ch)
}

/// Whether `tt` is the identifier `word`.
#[inline]
#[must_use]
pub fn is_keyword(tt: &TokenTree, word: &str) -> bool {
    matches!(tt, TokenTree::Ident(i) if i.to_string() == word)
}

/// What [`Cursor::take_expr`] expects next at the expression's top level,
/// which decides what a `<` there is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Due {
    /// An operand: at the start, after a binary or prefix operator, `::`, or a
    /// keyword that takes an expression. A `<` opens a qualified path.
    Operand,
    /// An operator: an operand has been read. A `<` compares or shifts.
    Operator,
    /// The start of the type after `as` or `->`: `&`, `*`, `mut`, `const`, a
    /// lifetime, then a path. A `<` opens a qualified path.
    TypeStart,
    /// Inside that type's path. A `<` opens its generic arguments.
    TypePath,
}

/// The keywords an operand follows: `if <T as Tr>::C == 1 { .. }`.
const TAKES_OPERAND: &[&str] = &[
    "async", "box", "break", "const", "continue", "else", "for", "if", "in", "let", "loop",
    "match", "move", "mut", "ref", "return", "static", "unsafe", "while", "yield",
];

/// Splits `tokens` at the top-level commas, keeping no empty trailing piece.
///
/// Top level is outside every `<...>` the tokens open (see
/// [`Cursor::take_run`]).
#[inline]
#[must_use]
pub fn split_commas(tokens: Vec<TokenTree>) -> Vec<Vec<TokenTree>> {
    split_top(tokens, ',')
}

/// Splits `tokens` at the top-level `separator` punctuation (see
/// [`split_commas`]).
#[must_use]
pub fn split_top(tokens: Vec<TokenTree>, separator: char) -> Vec<Vec<TokenTree>> {
    let mut reader = Cursor::new(tokens.into_iter().collect(), Span::call_site());
    let mut out = Vec::new();
    loop {
        let piece = reader.take_run(|tt, top| top && is_punct(tt, separator));
        let comma = reader.next_raw();
        if !piece.is_empty() || comma.is_some() {
            out.push(piece);
        }
        if comma.is_none() {
            break;
        }
    }
    if out.last().is_some_and(Vec::is_empty) {
        out.pop();
    }
    out
}
