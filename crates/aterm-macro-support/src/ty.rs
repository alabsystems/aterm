// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Type syntax tree for derive analysis (parameter bounds, lifetimes, slices), falling back to [`Ty::Other`].

use proc_macro::{Delimiter, Ident, Spacing, TokenStream, TokenTree};

use crate::cursor::{is_keyword, is_punct, split_commas, split_top};
use crate::item::Lifetime;

/// Parsed Rust type for derive analysis.
#[derive(Clone, Debug)]
pub enum Ty {
    /// `a::B<C>`, `<T as Trait>::Assoc`.
    Path(TyPath),
    /// `&'a mut T`.
    Reference {
        lifetime: Option<Lifetime>,
        mutable: bool,
        elem: Box<Ty>,
    },
    /// `*const T`, `*mut T`.
    Ptr(Box<Ty>),
    /// `[T]`.
    Slice(Box<Ty>),
    /// `[T; N]`.
    Array(Box<Ty>),
    /// `(A, B)`, `()`.
    Tuple(Vec<Ty>),
    /// `(T)`.
    Paren(Box<Ty>),
    /// None-delimited group (`$t:ty` fragment).
    Group(Box<Ty>),
    /// `fn(A) -> B`.
    BareFn {
        inputs: Vec<Ty>,
        output: Option<Box<Ty>>,
    },
    /// `dyn A + B`.
    TraitObject(Vec<Bound>),
    /// `impl A + B`.
    ImplTrait(Vec<Bound>),
    /// `m!(...)`.
    Macro(TokenStream),
    /// `!`.
    Never,
    /// `_`.
    Infer,
    /// Unrecognized type syntax.
    Other,
}

/// Path type, optionally qualified (`<T as Trait>::Assoc`).
#[derive(Clone, Debug)]
pub struct TyPath {
    /// `T` in `<T as Trait>::Assoc` or `<T>::Assoc`.
    pub qself: Option<Box<Ty>>,
    /// Whether the path starts with `::`.
    pub leading_colon: bool,
    /// The segments; for a qualified path, the trait's and then the rest.
    pub segments: Vec<Segment>,
}

/// Path segment with optional generic arguments.
#[derive(Clone, Debug)]
pub struct Segment {
    /// Its name.
    pub ident: Ident,
    /// Its arguments.
    pub args: PathArgs,
}

/// Generic arguments on a [`Segment`].
#[derive(Clone, Debug)]
pub enum PathArgs {
    /// None.
    None,
    /// `<A, B>`.
    Angle(Vec<GenericArg>),
    /// `(A, B) -> C` (`Fn`-trait sugar).
    Paren {
        inputs: Vec<Ty>,
        output: Option<Box<Ty>>,
    },
}

/// Generic argument inside `<...>`.
#[derive(Clone, Debug)]
pub enum GenericArg {
    /// `'a`.
    Lifetime(Lifetime),
    /// A type.
    Type(Ty),
    /// `Item = T`.
    AssocType(Ident, Ty),
    /// Const argument or constraint (`Item: Bound`).
    Other,
}

/// Trait or lifetime bound in `dyn` or `impl`.
#[derive(Clone, Debug)]
pub enum Bound {
    /// A trait: its path.
    Trait(TyPath),
    /// A lifetime or anything else.
    Other,
}

/// Parse `tokens` as a [`Ty`], returning [`Ty::Other`] if trailing tokens remain.
#[inline]
#[must_use]
pub fn parse_type(tokens: &[TokenTree]) -> Ty {
    let mut p = Reader { toks: tokens, i: 0 };
    let ty = p.ty();
    if p.i == tokens.len() { ty } else { Ty::Other }
}

struct Reader<'a> {
    toks: &'a [TokenTree],
    i: usize,
}

impl<'a> Reader<'a> {
    #[inline]
    fn peek(&self) -> Option<&'a TokenTree> {
        self.toks.get(self.i)
    }

    #[inline]
    fn peek_punct(&self, ch: char) -> bool {
        self.peek().is_some_and(|tt| is_punct(tt, ch))
    }

    #[inline]
    fn peek_keyword(&self, word: &str) -> bool {
        self.peek().is_some_and(|tt| is_keyword(tt, word))
    }

    #[inline]
    fn peek_colon2(&self) -> bool {
        matches!(self.toks.get(self.i), Some(TokenTree::Punct(p)) if p.as_char() == ':' && p.spacing() == Spacing::Joint)
            && matches!(self.toks.get(self.i + 1), Some(tt) if is_punct(tt, ':'))
    }

    #[inline]
    fn peek_arrow(&self) -> bool {
        matches!(self.toks.get(self.i), Some(TokenTree::Punct(p)) if p.as_char() == '-' && p.spacing() == Spacing::Joint)
            && matches!(self.toks.get(self.i + 1), Some(tt) if is_punct(tt, '>'))
    }

    fn ty(&mut self) -> Ty {
        let Some(tt) = self.peek() else {
            return Ty::Other;
        };
        match tt {
            TokenTree::Group(g) => {
                let inner: Vec<TokenTree> = g.stream().into_iter().collect();
                let delimiter = g.delimiter();
                self.i += 1;
                match delimiter {
                    Delimiter::None => Ty::Group(Box::new(parse_type(&inner))),
                    Delimiter::Parenthesis => {
                        let ends_with_comma = inner.last().is_some_and(|t| is_punct(t, ','));
                        let elems = split_commas(inner);
                        if elems.len() == 1 && !ends_with_comma {
                            Ty::Paren(Box::new(parse_type(&elems[0])))
                        } else {
                            Ty::Tuple(elems.iter().map(|e| parse_type(e)).collect())
                        }
                    }
                    Delimiter::Bracket => {
                        let parts = split_top(inner, ';');
                        match parts.as_slice() {
                            [elem] => Ty::Slice(Box::new(parse_type(elem))),
                            [elem, _len] => Ty::Array(Box::new(parse_type(elem))),
                            _ => Ty::Other,
                        }
                    }
                    Delimiter::Brace => Ty::Other,
                }
            }
            TokenTree::Punct(p) => match p.as_char() {
                '&' => {
                    self.i += 1;
                    if self.peek_punct('&') {
                        let inner = self.reference();
                        return Ty::Reference {
                            lifetime: None,
                            mutable: false,
                            elem: Box::new(inner),
                        };
                    }
                    self.reference_rest()
                }
                '*' => {
                    self.i += 1;
                    if self.peek_keyword("const") || self.peek_keyword("mut") {
                        self.i += 1;
                    }
                    Ty::Ptr(Box::new(self.ty()))
                }
                '!' => {
                    self.i += 1;
                    Ty::Never
                }
                '<' | ':' => self.path().map_or(Ty::Other, |path| self.after_path(path)),
                _ => Ty::Other,
            },
            TokenTree::Ident(ident) => match ident.to_string().as_str() {
                "_" => {
                    self.i += 1;
                    Ty::Infer
                }
                "dyn" => {
                    self.i += 1;
                    Ty::TraitObject(self.bounds())
                }
                "impl" => {
                    self.i += 1;
                    Ty::ImplTrait(self.bounds())
                }
                "fn" | "unsafe" | "extern" | "for" => self.bare_fn(),
                _ => self.path().map_or(Ty::Other, |path| self.after_path(path)),
            },
            TokenTree::Literal(_) => Ty::Other,
        }
    }

    fn reference(&mut self) -> Ty {
        self.i += 1;
        self.reference_rest()
    }

    fn reference_rest(&mut self) -> Ty {
        let mut lifetime = None;
        if self.peek_punct('\'')
            && let (Some(TokenTree::Punct(apostrophe)), Some(TokenTree::Ident(ident))) =
                (self.toks.get(self.i), self.toks.get(self.i + 1))
        {
            lifetime = Some(Lifetime {
                apostrophe: apostrophe.span(),
                ident: ident.clone(),
            });
            self.i += 2;
        }
        let mutable = if self.peek_keyword("mut") {
            self.i += 1;
            true
        } else {
            false
        };
        Ty::Reference {
            lifetime,
            mutable,
            elem: Box::new(self.ty()),
        }
    }

    fn after_path(&mut self, path: TyPath) -> Ty {
        if self.peek_punct('!')
            && let Some(TokenTree::Group(g)) = self.toks.get(self.i + 1)
        {
            let tokens = g.stream();
            self.i += 2;
            return Ty::Macro(tokens);
        }
        Ty::Path(path)
    }

    fn path(&mut self) -> Option<TyPath> {
        let mut qself = None;
        let mut leading_colon = false;
        let mut segments = Vec::new();
        if self.peek_punct('<') {
            let close = self.matching_angle(self.i)?;
            let inner = &self.toks[self.i + 1..close];
            let as_at = top_level_keyword(inner, "as");
            let (self_ty, trait_toks) = match as_at {
                Some(at) => (&inner[..at], Some(&inner[at + 1..])),
                None => (inner, None),
            };
            qself = Some(Box::new(parse_type(self_ty)));
            match trait_toks {
                Some(toks) => {
                    let mut trait_reader = Reader { toks, i: 0 };
                    let trait_path = trait_reader.path()?;
                    segments.extend(trait_path.segments);
                }
                None => leading_colon = true,
            }
            self.i = close + 1;
            if !self.peek_colon2() {
                return None;
            }
            self.i += 2;
        } else if self.peek_colon2() {
            leading_colon = true;
            self.i += 2;
        }
        loop {
            let Some(TokenTree::Ident(ident)) = self.peek() else {
                return None;
            };
            let ident = ident.clone();
            self.i += 1;
            let mut args = PathArgs::None;
            if self.peek_colon2()
                && matches!(self.toks.get(self.i + 2), Some(tt) if is_punct(tt, '<'))
            {
                self.i += 2;
            }
            if self.peek_punct('<') {
                let close = self.matching_angle(self.i)?;
                let inner = self.toks[self.i + 1..close].to_vec();
                args =
                    PathArgs::Angle(split_commas(inner).iter().map(|a| generic_arg(a)).collect());
                self.i = close + 1;
            } else if let Some(TokenTree::Group(g)) = self.peek()
                && g.delimiter() == Delimiter::Parenthesis
            {
                let inner: Vec<TokenTree> = g.stream().into_iter().collect();
                self.i += 1;
                let inputs = split_commas(inner).iter().map(|a| parse_type(a)).collect();
                let output = self.output();
                args = PathArgs::Paren { inputs, output };
            }
            segments.push(Segment { ident, args });
            if self.peek_colon2() {
                self.i += 2;
            } else {
                break;
            }
        }
        Some(TyPath {
            qself,
            leading_colon,
            segments,
        })
    }

    fn output(&mut self) -> Option<Box<Ty>> {
        if self.peek_arrow() {
            self.i += 2;
            Some(Box::new(self.ty()))
        } else {
            None
        }
    }

    fn bare_fn(&mut self) -> Ty {
        if self.peek_keyword("for") {
            self.i += 1;
            let Some(close) = self.matching_angle(self.i) else {
                return Ty::Other;
            };
            self.i = close + 1;
        }
        if self.peek_keyword("unsafe") {
            self.i += 1;
        }
        if self.peek_keyword("extern") {
            self.i += 1;
            if matches!(self.peek(), Some(TokenTree::Literal(_))) {
                self.i += 1;
            }
        }
        if !self.peek_keyword("fn") {
            return Ty::Other;
        }
        self.i += 1;
        let Some(TokenTree::Group(g)) = self.peek() else {
            return Ty::Other;
        };
        let inner: Vec<TokenTree> = g.stream().into_iter().collect();
        self.i += 1;
        let inputs = split_commas(inner)
            .iter()
            .map(|arg| {
                let named = matches!(arg.first(), Some(TokenTree::Ident(_)))
                    && matches!(arg.get(1), Some(TokenTree::Punct(p)) if p.as_char() == ':' && p.spacing() == Spacing::Alone);
                if named {
                    parse_type(&arg[2..])
                } else {
                    parse_type(arg)
                }
            })
            .collect();
        let output = self.output();
        Ty::BareFn { inputs, output }
    }

    fn bounds(&mut self) -> Vec<Bound> {
        let rest = self.toks[self.i..].to_vec();
        self.i = self.toks.len();
        split_top(rest, '+')
            .iter()
            .map(|bound| {
                let mut toks: &[TokenTree] = bound;
                if let Some(TokenTree::Group(g)) = toks.first()
                    && g.delimiter() == Delimiter::Parenthesis
                    && toks.len() == 1
                {
                    let inner: Vec<TokenTree> = g.stream().into_iter().collect();
                    return bound_of(&inner);
                }
                if toks.first().is_some_and(|t| is_punct(t, '?')) {
                    toks = &toks[1..];
                }
                bound_of(toks)
            })
            .collect()
    }

    /// Return the index of the `>` closing the `<` at `open`, ignoring `->`.
    fn matching_angle(&self, open: usize) -> Option<usize> {
        let mut depth = 0usize;
        let mut after_joint_minus = false;
        for (i, tt) in self.toks.iter().enumerate().skip(open) {
            let mut joint_minus = false;
            if let TokenTree::Punct(p) = tt {
                match p.as_char() {
                    '<' => depth += 1,
                    '>' if !after_joint_minus => {
                        depth = depth.checked_sub(1)?;
                        if depth == 0 {
                            return Some(i);
                        }
                    }
                    '-' => joint_minus = p.spacing() == Spacing::Joint,
                    _ => {}
                }
            }
            after_joint_minus = joint_minus;
        }
        None
    }
}

fn bound_of(toks: &[TokenTree]) -> Bound {
    let mut reader = Reader { toks, i: 0 };
    if reader.peek_keyword("for") {
        reader.i += 1;
        let Some(close) = reader.matching_angle(reader.i) else {
            return Bound::Other;
        };
        reader.i = close + 1;
    }
    if reader.peek_punct('\'') {
        return Bound::Other;
    }
    match reader.path() {
        Some(path) if reader.i == toks.len() => Bound::Trait(path),
        _ => Bound::Other,
    }
}

fn generic_arg(tokens: &[TokenTree]) -> GenericArg {
    match tokens {
        [TokenTree::Punct(p), TokenTree::Ident(ident)] if p.as_char() == '\'' => {
            GenericArg::Lifetime(Lifetime {
                apostrophe: p.span(),
                ident: ident.clone(),
            })
        }
        [TokenTree::Literal(_)] => GenericArg::Other,
        [TokenTree::Group(g)] if g.delimiter() == Delimiter::Brace => GenericArg::Other,
        [TokenTree::Punct(p), TokenTree::Literal(_)] if p.as_char() == '-' => GenericArg::Other,
        [TokenTree::Ident(ident), TokenTree::Punct(eq), rest @ ..]
            if eq.as_char() == '=' && eq.spacing() == Spacing::Alone =>
        {
            GenericArg::AssocType(ident.clone(), parse_type(rest))
        }
        [TokenTree::Ident(_), TokenTree::Punct(colon), ..]
            if colon.as_char() == ':' && colon.spacing() == Spacing::Alone =>
        {
            GenericArg::Other
        }
        _ => {
            // `Assoc<'a> = T`, a generic associated type's binding.
            if let Some(TokenTree::Ident(ident)) = tokens.first()
                && let Some(eq) = top_level_eq(tokens)
            {
                return GenericArg::AssocType(ident.clone(), parse_type(&tokens[eq + 1..]));
            }
            GenericArg::Type(parse_type(tokens))
        }
    }
}

fn top_level_keyword(tokens: &[TokenTree], word: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (i, tt) in tokens.iter().enumerate() {
        match tt {
            TokenTree::Punct(p) if p.as_char() == '<' => depth += 1,
            TokenTree::Punct(p) if p.as_char() == '>' => depth = depth.saturating_sub(1),
            TokenTree::Ident(id) if depth == 0 && id.to_string() == word => return Some(i),
            _ => {}
        }
    }
    None
}

fn top_level_eq(tokens: &[TokenTree]) -> Option<usize> {
    let mut depth = 0usize;
    for (i, tt) in tokens.iter().enumerate() {
        if let TokenTree::Punct(p) = tt {
            match p.as_char() {
                '<' => depth += 1,
                '>' => depth = depth.saturating_sub(1),
                '=' if depth == 0 && p.spacing() == Spacing::Alone => return Some(i),
                _ => {}
            }
        }
    }
    None
}
