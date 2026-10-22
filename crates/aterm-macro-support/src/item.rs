// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The item a derive receives: a struct, an enum or a union.
//!
//! The grammar is the Reference's (Items > Structs, Enumerations, Unions,
//! Generic parameters, Where clauses, Attributes, Visibility). Attributes
//! arrive with `cfg`/`cfg_attr` already applied and doc comments as
//! `#[doc = "..."]`. Field types, bounds, defaults and discriminants are kept
//! as the token runs they were written as; [`crate::parse_type`] reads a type
//! when a macro needs to look inside one.

use proc_macro::{Delimiter, Group, Ident, Punct, Spacing, Span, TokenStream, TokenTree};

use crate::cursor::{Cursor, is_keyword, is_punct, split_top};
use crate::error::Error;
use crate::quote::ToTokens;

/// A lifetime, `'a`: an apostrophe joint to an identifier.
#[derive(Clone, Debug)]
pub struct Lifetime {
    /// The apostrophe's span.
    pub apostrophe: Span,
    /// The name without its apostrophe.
    pub ident: Ident,
}

impl Lifetime {
    /// `'name` at `span`; `name` is written without the apostrophe.
    #[inline]
    #[must_use]
    pub fn new(name: &str, span: Span) -> Self {
        Lifetime {
            apostrophe: span,
            ident: Ident::new(name, span),
        }
    }

    /// The lifetime as written, apostrophe included.
    #[inline]
    #[must_use]
    pub fn name(&self) -> String {
        format!("'{}", self.ident)
    }
}

impl ToTokens for Lifetime {
    fn to_tokens(&self, out: &mut Vec<TokenTree>) {
        let mut apostrophe = Punct::new('\'', Spacing::Joint);
        apostrophe.set_span(self.apostrophe);
        out.push(TokenTree::Punct(apostrophe));
        out.push(TokenTree::Ident(self.ident.clone()));
    }
}

/// An outer attribute, `#[path args]`.
#[derive(Clone, Debug)]
pub struct Attribute {
    /// The `#`.
    pub pound: Punct,
    /// The `[...]`, as written.
    pub bracket: Group,
    /// The path's segments (`serde`, `doc`, `rustfmt::skip`).
    pub path: Vec<Ident>,
    /// What follows the path.
    pub args: AttrArgs,
}

/// What follows an attribute's path.
#[derive(Clone, Debug)]
pub enum AttrArgs {
    /// `#[path]`.
    None,
    /// `#[path(...)]`, `#[path[...]]` or `#[path{...}]`.
    List(Group),
    /// `#[path = value]`: the value's tokens.
    Value(Vec<TokenTree>),
}

impl Attribute {
    /// Whether the path is the single segment `name`.
    #[inline]
    #[must_use]
    pub fn is(&self, name: &str) -> bool {
        self.path.len() == 1 && self.path[0].to_string() == name
    }

    /// The span of the `#`.
    #[inline]
    #[must_use]
    pub fn first_span(&self) -> Span {
        self.pound.span()
    }

    /// The span of the `[...]`.
    #[inline]
    #[must_use]
    pub fn last_span(&self) -> Span {
        self.bracket.span()
    }
}

impl ToTokens for Attribute {
    fn to_tokens(&self, out: &mut Vec<TokenTree>) {
        out.push(TokenTree::Punct(self.pound.clone()));
        out.push(TokenTree::Group(self.bracket.clone()));
    }
}

/// A generic parameter.
#[derive(Clone, Debug)]
pub enum GenericParam {
    /// `'a: 'b + 'c`.
    Lifetime {
        /// Its outer attributes.
        attrs: Vec<Attribute>,
        /// The lifetime.
        lifetime: Lifetime,
        /// The lifetimes it outlives.
        bounds: Vec<Lifetime>,
    },
    /// `T: Bound + Bound = Default`.
    Type {
        /// Its outer attributes.
        attrs: Vec<Attribute>,
        /// The name.
        ident: Ident,
        /// Each `+`-separated bound, as written.
        bounds: Vec<TokenStream>,
        /// The default, as written.
        default: Option<TokenStream>,
    },
    /// `const N: Ty = Default`.
    Const {
        /// Its outer attributes.
        attrs: Vec<Attribute>,
        /// The `const` keyword.
        const_token: Ident,
        /// The name.
        ident: Ident,
        /// The type, as written.
        ty: TokenStream,
        /// The default, as written.
        default: Option<TokenStream>,
    },
}

/// An item's generic parameters and where clause.
#[derive(Clone, Debug, Default)]
pub struct Generics {
    /// The parameters, in the order written.
    pub params: Vec<GenericParam>,
    /// The where clause's predicates, each as written.
    pub where_predicates: Vec<TokenStream>,
}

fn punct(ch: char, spacing: Spacing) -> TokenTree {
    TokenTree::Punct(Punct::new(ch, spacing))
}

impl Generics {
    /// The names of the type parameters, in order.
    pub fn type_params(&self) -> impl Iterator<Item = &Ident> {
        self.params.iter().filter_map(|p| match p {
            GenericParam::Type { ident, .. } => Some(ident),
            _ => None,
        })
    }

    /// The lifetime parameters, in order.
    pub fn lifetimes(&self) -> impl Iterator<Item = &Lifetime> {
        self.params.iter().filter_map(|p| match p {
            GenericParam::Lifetime { lifetime, .. } => Some(lifetime),
            _ => None,
        })
    }

    /// Whether there are no parameters.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.params.is_empty()
    }

    /// The parameter list for `impl<...>`: bounds kept, defaults dropped,
    /// lifetimes first. Empty when there are no parameters.
    #[inline]
    #[must_use]
    pub fn impl_generics(&self) -> TokenStream {
        self.list(true)
    }

    /// The argument list for the type, `<'a, T, N>`. Empty when there are no
    /// parameters.
    #[inline]
    #[must_use]
    pub fn ty_generics(&self) -> TokenStream {
        self.list(false)
    }

    /// `where p, q,`; empty when there are no predicates.
    #[must_use]
    pub fn where_clause(&self) -> TokenStream {
        if self.where_predicates.is_empty() {
            return TokenStream::new();
        }
        let mut out = vec![TokenTree::Ident(Ident::new("where", Span::call_site()))];
        for predicate in &self.where_predicates {
            out.extend(predicate.clone());
            out.push(punct(',', Spacing::Alone));
        }
        out.into_iter().collect()
    }

    fn list(&self, with_bounds: bool) -> TokenStream {
        if self.params.is_empty() {
            return TokenStream::new();
        }
        let lifetimes = self
            .params
            .iter()
            .filter(|p| matches!(p, GenericParam::Lifetime { .. }));
        let rest = self
            .params
            .iter()
            .filter(|p| !matches!(p, GenericParam::Lifetime { .. }));
        let mut out = vec![punct('<', Spacing::Alone)];
        for param in lifetimes.chain(rest) {
            match param {
                GenericParam::Lifetime {
                    attrs,
                    lifetime,
                    bounds,
                } => {
                    if with_bounds {
                        attrs.to_tokens(&mut out);
                    }
                    lifetime.to_tokens(&mut out);
                    if with_bounds && !bounds.is_empty() {
                        out.push(punct(':', Spacing::Alone));
                        for (i, bound) in bounds.iter().enumerate() {
                            if i > 0 {
                                out.push(punct('+', Spacing::Alone));
                            }
                            bound.to_tokens(&mut out);
                        }
                    }
                }
                GenericParam::Type {
                    attrs,
                    ident,
                    bounds,
                    ..
                } => {
                    if with_bounds {
                        attrs.to_tokens(&mut out);
                    }
                    out.push(TokenTree::Ident(ident.clone()));
                    if with_bounds && !bounds.is_empty() {
                        out.push(punct(':', Spacing::Alone));
                        for (i, bound) in bounds.iter().enumerate() {
                            if i > 0 {
                                out.push(punct('+', Spacing::Alone));
                            }
                            out.extend(bound.clone());
                        }
                    }
                }
                GenericParam::Const {
                    attrs,
                    const_token,
                    ident,
                    ty,
                    ..
                } => {
                    if with_bounds {
                        attrs.to_tokens(&mut out);
                        out.push(TokenTree::Ident(const_token.clone()));
                    }
                    out.push(TokenTree::Ident(ident.clone()));
                    if with_bounds {
                        out.push(punct(':', Spacing::Alone));
                        out.extend(ty.clone());
                    }
                }
            }
            out.push(punct(',', Spacing::Alone));
        }
        out.push(punct('>', Spacing::Alone));
        out.into_iter().collect()
    }
}

/// A field of a struct, a variant or a union.
#[derive(Clone, Debug)]
pub struct Field {
    /// Its outer attributes.
    pub attrs: Vec<Attribute>,
    /// Its visibility, as written (empty when private).
    pub vis: Vec<TokenTree>,
    /// Its name; `None` in a tuple.
    pub ident: Option<Ident>,
    /// Its type, as written.
    pub ty: Vec<TokenTree>,
    /// The span of its first token (an attribute's `#` when it has one).
    pub first_span: Span,
    /// The span of its type's last token.
    pub last_span: Span,
}

impl Field {
    /// The type as a stream.
    #[inline]
    #[must_use]
    pub fn ty_stream(&self) -> TokenStream {
        self.ty.iter().cloned().collect()
    }
}

/// A struct's or a variant's fields.
#[derive(Clone, Debug)]
pub enum Fields {
    /// `{ a: A, b: B }`.
    Named(Vec<Field>),
    /// `(A, B)`.
    Unnamed(Vec<Field>),
    /// No fields.
    Unit,
}

impl Fields {
    /// The fields, in order; empty for a unit.
    #[inline]
    #[must_use]
    pub fn iter(&self) -> &[Field] {
        match self {
            Fields::Named(f) | Fields::Unnamed(f) => f,
            Fields::Unit => &[],
        }
    }
}

/// An enum variant.
#[derive(Clone, Debug)]
pub struct Variant {
    /// Its outer attributes.
    pub attrs: Vec<Attribute>,
    /// Its name.
    pub ident: Ident,
    /// Its fields.
    pub fields: Fields,
    /// `= expr`, the expression as written.
    pub discriminant: Option<Vec<TokenTree>>,
    /// The span of its first token.
    pub first_span: Span,
    /// The span of its last token.
    pub last_span: Span,
}

/// The body of the item.
#[derive(Clone, Debug)]
pub enum Data {
    /// A struct.
    Struct(Fields),
    /// An enum.
    Enum(Vec<Variant>),
    /// A union.
    Union(Vec<Field>),
}

/// The item a derive receives.
#[derive(Clone, Debug)]
pub struct DeriveInput {
    /// Its outer attributes.
    pub attrs: Vec<Attribute>,
    /// Its visibility, as written.
    pub vis: Vec<TokenTree>,
    /// `struct`, `enum` or `union`.
    pub keyword: Ident,
    /// Its name.
    pub ident: Ident,
    /// Its generics.
    pub generics: Generics,
    /// Its body.
    pub data: Data,
    /// The span of its first token.
    pub first_span: Span,
    /// The span of its last token.
    pub last_span: Span,
}

/// Parses the input of a `#[proc_macro_derive]`.
///
/// # Errors
/// Anything that is not a struct, an enum or a union, as the compiler would
/// hand it to a derive, at the first token that does not fit.
pub fn parse_derive_input(input: TokenStream) -> Result<DeriveInput, Error> {
    let mut c = Cursor::new(input, Span::call_site());
    let first_span = c.span();
    let attrs = parse_attrs(&mut c)?;
    let vis = parse_vis(&mut c);
    let keyword = c.expect_ident("`struct`, `enum` or `union`")?;
    let kind = keyword.to_string();
    if !matches!(kind.as_str(), "struct" | "enum" | "union") {
        return Err(Error::new(
            keyword.span(),
            "expected `struct`, `enum` or `union`",
        ));
    }
    let ident = c.expect_ident("the item's name")?;
    let mut generics = parse_generics(&mut c)?;
    let data;
    let last_span;
    match kind.as_str() {
        "struct" => {
            if c.peek_keyword("where") {
                generics.where_predicates = parse_where(&mut c);
            }
            if let Some(group) = c.eat_group(Delimiter::Brace) {
                last_span = group.span();
                data = Data::Struct(Fields::Named(parse_named(&group)?));
            } else if let Some(group) = c.eat_group(Delimiter::Parenthesis) {
                let fields = parse_unnamed(&group)?;
                if c.peek_keyword("where") {
                    generics.where_predicates = parse_where(&mut c);
                }
                let semi = c.expect_punct(';', "`;` after a tuple struct")?;
                last_span = semi.span();
                data = Data::Struct(Fields::Unnamed(fields));
            } else {
                let semi = c.expect_punct(';', "`{`, `(` or `;`")?;
                last_span = semi.span();
                data = Data::Struct(Fields::Unit);
            }
        }
        "enum" => {
            if c.peek_keyword("where") {
                generics.where_predicates = parse_where(&mut c);
            }
            let Some(group) = c.eat_group(Delimiter::Brace) else {
                return Err(Error::new(c.span(), "expected `{`"));
            };
            last_span = group.span();
            data = Data::Enum(parse_variants(&group)?);
        }
        _ => {
            if c.peek_keyword("where") {
                generics.where_predicates = parse_where(&mut c);
            }
            let Some(group) = c.eat_group(Delimiter::Brace) else {
                return Err(Error::new(c.span(), "expected `{`"));
            };
            last_span = group.span();
            data = Data::Union(parse_named(&group)?);
        }
    }
    if !c.is_empty() {
        return Err(Error::new(c.span(), "unexpected token after the item"));
    }
    Ok(DeriveInput {
        attrs,
        vis,
        keyword,
        ident,
        generics,
        data,
        first_span,
        last_span,
    })
}

/// Parses outer attributes, `#[...]`, while they come.
///
/// # Errors
/// A `#` not followed by `[...]`, or a bracket with no path.
pub fn parse_attrs(c: &mut Cursor) -> Result<Vec<Attribute>, Error> {
    let mut attrs = Vec::new();
    while c.peek_punct('#') {
        let Some(pound) = c.eat_punct('#') else { break };
        let Some(bracket) = c.eat_group(Delimiter::Bracket) else {
            return Err(Error::new(c.span(), "expected `[` after `#`"));
        };
        let mut inner = Cursor::of_group(&bracket);
        // `#[unsafe(no_mangle)]`: the path is inside.
        if inner.peek_keyword("unsafe")
            && matches!(inner.peek_raw_nth(1), Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Parenthesis)
        {
            inner.next();
            if let Some(TokenTree::Group(g)) = inner.next() {
                inner = Cursor::of_group(&g);
            }
        }
        let mut path = vec![inner.expect_ident("an attribute path")?];
        while inner.eat_op("::").is_some() {
            path.push(inner.expect_ident("an attribute path segment")?);
        }
        let args = if inner.is_empty() {
            AttrArgs::None
        } else if inner.eat_punct('=').is_some() {
            AttrArgs::Value(inner.rest())
        } else {
            match inner.next() {
                Some(TokenTree::Group(g)) if g.delimiter() != Delimiter::None => {
                    if !inner.is_empty() {
                        return Err(Error::new(inner.span(), "unexpected token in an attribute"));
                    }
                    AttrArgs::List(g)
                }
                Some(other) => {
                    return Err(Error::new(other.span(), "unexpected token in an attribute"));
                }
                None => AttrArgs::None,
            }
        };
        attrs.push(Attribute {
            pound,
            bracket,
            path,
            args,
        });
    }
    Ok(attrs)
}

/// Parses a visibility (`pub`, `pub(crate)`, `pub(self)`, `pub(super)`,
/// `pub(in path)`), possibly inside a `$vis` None-group; empty when private.
fn parse_vis(c: &mut Cursor) -> Vec<TokenTree> {
    // A None-group in front is a `$vis` fragment when it is empty or starts
    // with `pub`; otherwise it is a tuple field's `$t:ty`, which stays whole.
    if let Some(TokenTree::Group(g)) = c.peek_raw()
        && g.delimiter() == Delimiter::None
    {
        let mut inside = Cursor::of_group(g);
        let is_vis = inside.is_empty() || inside.peek_keyword("pub");
        if !is_vis {
            return Vec::new();
        }
        c.flatten();
    }
    let Some(kw) = c.eat_keyword("pub") else {
        return Vec::new();
    };
    let mut vis = vec![TokenTree::Ident(kw)];
    if let Some(TokenTree::Group(g)) = c.peek()
        && g.delimiter() == Delimiter::Parenthesis
    {
        let mut inside = Cursor::of_group(g);
        let restricted = match inside.next() {
            Some(first) if is_keyword(&first, "in") => true,
            Some(first)
                if is_keyword(&first, "crate")
                    || is_keyword(&first, "self")
                    || is_keyword(&first, "super") =>
            {
                inside.is_empty()
            }
            _ => false,
        };
        if restricted && let Some(group) = c.next() {
            vis.push(group);
        }
    }
    vis
}

/// Parses `<...>` if it comes next.
fn parse_generics(c: &mut Cursor) -> Result<Generics, Error> {
    let mut generics = Generics::default();
    if c.eat_punct('<').is_none() {
        return Ok(generics);
    }
    loop {
        if c.eat_punct('>').is_some() {
            break;
        }
        let attrs = parse_attrs(c)?;
        if c.peek_punct('\'') {
            let lifetime = parse_lifetime(c)?;
            let mut bounds = Vec::new();
            if c.eat_punct(':').is_some() {
                while c.peek_punct('\'') {
                    bounds.push(parse_lifetime(c)?);
                    if c.eat_punct('+').is_none() {
                        break;
                    }
                }
            }
            generics.params.push(GenericParam::Lifetime {
                attrs,
                lifetime,
                bounds,
            });
        } else if let Some(const_token) = c.eat_keyword("const") {
            let ident = c.expect_ident("a const parameter's name")?;
            c.expect_punct(':', "`:` and a const parameter's type")?;
            let ty = take_param_run(c, true);
            let default = if c.eat_punct('=').is_some() {
                Some(take_param_run(c, false).into_iter().collect())
            } else {
                None
            };
            generics.params.push(GenericParam::Const {
                attrs,
                const_token,
                ident,
                ty: ty.into_iter().collect(),
                default,
            });
        } else {
            let ident = c.expect_ident("a generic parameter")?;
            let mut bounds = Vec::new();
            if c.eat_punct(':').is_some() {
                let run = take_param_run(c, true);
                bounds = split_top(run, '+')
                    .into_iter()
                    .filter(|b| !b.is_empty())
                    .map(|b| b.into_iter().collect())
                    .collect();
            }
            let default = if c.eat_punct('=').is_some() {
                Some(take_param_run(c, false).into_iter().collect())
            } else {
                None
            };
            generics.params.push(GenericParam::Type {
                attrs,
                ident,
                bounds,
                default,
            });
        }
        if c.eat_punct(',').is_none() {
            c.expect_punct('>', "`,` or `>` in the generic parameters")?;
            break;
        }
    }
    Ok(generics)
}

/// A parameter's bounds, type or default: up to the top-level `,` or the `>`
/// that closes the list, and, when `stop_at_eq`, a top-level `=`.
fn take_param_run(c: &mut Cursor, stop_at_eq: bool) -> Vec<TokenTree> {
    c.take_run(|tt, top| {
        top && (is_punct(tt, ',') || is_punct(tt, '>') || (stop_at_eq && is_punct(tt, '=')))
    })
}

/// Parses `'name`.
fn parse_lifetime(c: &mut Cursor) -> Result<Lifetime, Error> {
    let apostrophe = c.expect_punct('\'', "a lifetime")?;
    let ident = c.expect_ident("a lifetime's name")?;
    Ok(Lifetime {
        apostrophe: apostrophe.span(),
        ident,
    })
}

/// Parses `where p, q` up to the body's `{` or the `;`.
fn parse_where(c: &mut Cursor) -> Vec<TokenStream> {
    c.eat_keyword("where");
    let mut predicates = Vec::new();
    loop {
        let run = c.take_run(|tt, top| {
            top && (is_punct(tt, ',')
                || is_punct(tt, ';')
                || matches!(tt, TokenTree::Group(g) if g.delimiter() == Delimiter::Brace))
        });
        if !run.is_empty() {
            predicates.push(run.into_iter().collect());
        }
        if c.eat_punct(',').is_none() {
            break;
        }
    }
    predicates
}

/// Parses `{ a: A, ... }`.
fn parse_named(group: &Group) -> Result<Vec<Field>, Error> {
    let mut c = Cursor::of_group(group);
    let mut fields = Vec::new();
    while !c.is_empty() {
        let first_span = c.span();
        let attrs = parse_attrs(&mut c)?;
        let vis = parse_vis(&mut c);
        let ident = c.expect_ident("a field name")?;
        c.expect_punct(':', "`:` after a field name")?;
        let ty = c.take_run(|tt, top| top && is_punct(tt, ','));
        let Some(last) = ty.last() else {
            return Err(Error::new(c.span(), "expected a field type"));
        };
        let last_span = last.span();
        fields.push(Field {
            attrs,
            vis,
            ident: Some(ident),
            ty,
            first_span,
            last_span,
        });
        if c.eat_punct(',').is_none() && !c.is_empty() {
            return Err(Error::new(c.span(), "expected `,`"));
        }
    }
    Ok(fields)
}

/// Parses `(A, ...)`.
fn parse_unnamed(group: &Group) -> Result<Vec<Field>, Error> {
    let mut c = Cursor::of_group(group);
    let mut fields = Vec::new();
    while !c.is_empty() {
        let first_span = c.span();
        let attrs = parse_attrs(&mut c)?;
        let vis = parse_vis(&mut c);
        let ty = c.take_run(|tt, top| top && is_punct(tt, ','));
        let Some(last) = ty.last() else {
            return Err(Error::new(c.span(), "expected a field type"));
        };
        let last_span = last.span();
        fields.push(Field {
            attrs,
            vis,
            ident: None,
            ty,
            first_span,
            last_span,
        });
        if c.eat_punct(',').is_none() && !c.is_empty() {
            return Err(Error::new(c.span(), "expected `,`"));
        }
    }
    Ok(fields)
}

/// Parses `{ A, B(..), C { .. } = 3, ... }`.
fn parse_variants(group: &Group) -> Result<Vec<Variant>, Error> {
    let mut c = Cursor::of_group(group);
    let mut variants = Vec::new();
    while !c.is_empty() {
        let first_span = c.span();
        let attrs = parse_attrs(&mut c)?;
        // The compiler rejects a visibility here; it parses one first.
        let _ = parse_vis(&mut c);
        let ident = c.expect_ident("a variant name")?;
        let mut last_span = ident.span();
        let fields = if let Some(g) = c.eat_group(Delimiter::Brace) {
            last_span = g.span();
            Fields::Named(parse_named(&g)?)
        } else if let Some(g) = c.eat_group(Delimiter::Parenthesis) {
            last_span = g.span();
            Fields::Unnamed(parse_unnamed(&g)?)
        } else {
            Fields::Unit
        };
        let discriminant = if let Some(eq) = c.eat_punct('=') {
            // An expression, not a type: `A = 1 << 0, B` has two variants.
            let run = c.take_expr()?;
            let Some(last) = run.last() else {
                return Err(Error::new(eq.span(), "expected a discriminant after `=`"));
            };
            last_span = last.span();
            Some(run)
        } else {
            None
        };
        variants.push(Variant {
            attrs,
            ident,
            fields,
            discriminant,
            first_span,
            last_span,
        });
        if c.eat_punct(',').is_none() && !c.is_empty() {
            return Err(Error::new(c.span(), "expected `,`"));
        }
    }
    Ok(variants)
}
