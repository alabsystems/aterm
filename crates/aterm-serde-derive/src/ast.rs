// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde_derive 1.0.228 src/internals/ast.rs,
// src/internals/check.rs and src/internals/receiver.rs, and modified by the
// aterm project in 2026: built on aterm-macro-support's DeriveInput, and only
// the checks that can fire on aterm's attribute set.
// See ../LICENSE-MIT and ../LICENSE-APACHE for the upstream licenses.

//! A Serde AST, parsed from the derive input and ready to generate Rust code.

use aterm_macro_support::{
    Data as InputData, DeriveInput, Errors, Fields, GenericParam, Generics, ToTokens, Ty,
    parse_type,
};
use proc_macro::{Ident, Literal, Punct, Spacing, Span, TokenStream, TokenTree};

use crate::attr;

/// A source data structure annotated with `#[derive(Serialize)]` and/or `#[derive(Deserialize)]`.
pub struct Container<'a> {
    /// The struct or enum name (without generics).
    pub ident: Ident,
    /// Attributes on the structure, parsed for Serde.
    pub attrs: attr::Container,
    /// The contents of the struct or enum.
    pub data: Data<'a>,
    /// Any generics on the struct or enum.
    pub generics: &'a Generics,
    /// Original input.
    pub original: &'a DeriveInput,
}

/// The fields of a struct or enum.
pub enum Data<'a> {
    Enum(Vec<Variant<'a>>),
    Struct(Style, Vec<Field<'a>>),
}

/// A variant of an enum.
pub struct Variant<'a> {
    pub ident: Ident,
    pub attrs: attr::Variant,
    pub style: Style,
    pub fields: Vec<Field<'a>>,
}

/// A field of a struct.
pub struct Field<'a> {
    pub member: Member,
    pub attrs: attr::Field,
    /// The type as written.
    pub ty: TokenStream,
    /// The type, read for analysis.
    pub ty_tree: Ty,
    pub original: &'a aterm_macro_support::Field,
}

/// A field's name in a member expression.
#[derive(Clone)]
pub enum Member {
    Named(Ident),
    Unnamed(u32),
}

impl ToTokens for Member {
    fn to_tokens(&self, out: &mut Vec<TokenTree>) {
        match self {
            Member::Named(ident) => out.push(TokenTree::Ident(ident.clone())),
            Member::Unnamed(index) => {
                out.push(TokenTree::Literal(Literal::u32_unsuffixed(*index)));
            }
        }
    }
}

#[derive(Copy, Clone)]
pub enum Style {
    /// Named fields.
    Struct,
    /// Many unnamed fields.
    Tuple,
    /// One unnamed field.
    Newtype,
    /// No fields.
    Unit,
}

/// Which derive is being expanded.
#[derive(Copy, Clone)]
pub enum Derive {
    Serialize,
    Deserialize,
}

impl<'a> Container<'a> {
    /// Convert the raw derive input into a Serde-specific representation.
    pub fn from_ast(cx: &Errors, item: &'a DeriveInput, derive: Derive) -> Option<Container<'a>> {
        let attrs = attr::Container::from_ast(cx, item);

        let mut data = match &item.data {
            InputData::Enum(variants) => Data::Enum(enum_from_ast(cx, variants, attrs.default())),
            InputData::Struct(fields) => {
                let (style, fields) = struct_from_ast(cx, fields, attrs.default());
                Data::Struct(style, fields)
            }
            InputData::Union(_) => {
                cx.spanned(
                    item.first_span,
                    item.last_span,
                    "Serde does not support derive for unions",
                );
                return None;
            }
        };

        match &mut data {
            Data::Enum(variants) => {
                for variant in variants {
                    variant.attrs.rename_by_rules(attrs.rename_all_rule());
                    for field in &mut variant.fields {
                        field.attrs.rename_by_rules(attr::RenameRule::None);
                    }
                }
            }
            Data::Struct(_, fields) => {
                for field in fields {
                    field.attrs.rename_by_rules(attrs.rename_all_rule());
                }
            }
        }

        let mut item = Container {
            ident: item.ident.clone(),
            attrs,
            data,
            generics: &item.generics,
            original: item,
        };
        check(cx, &mut item, derive);
        Some(item)
    }

    /// An error over the whole item.
    pub fn error(&self, cx: &Errors, message: impl std::fmt::Display) {
        cx.spanned(self.original.first_span, self.original.last_span, message);
    }
}

impl<'a> Data<'a> {
    pub fn all_fields(&'a self) -> Box<dyn Iterator<Item = &'a Field<'a>> + 'a> {
        match self {
            Data::Enum(variants) => {
                Box::new(variants.iter().flat_map(|variant| variant.fields.iter()))
            }
            Data::Struct(_, fields) => Box::new(fields.iter()),
        }
    }
}

impl Field<'_> {
    /// The span upstream attaches to code generated for this field.
    pub const fn span(&self) -> Span {
        self.original.first_span
    }
}

fn enum_from_ast<'a>(
    cx: &Errors,
    variants: &'a [aterm_macro_support::Variant],
    container_default: &attr::Default,
) -> Vec<Variant<'a>> {
    variants
        .iter()
        .map(|variant| {
            let attrs = attr::Variant::from_ast(cx, variant);
            let (style, fields) = struct_from_ast(cx, &variant.fields, container_default);
            Variant {
                ident: variant.ident.clone(),
                attrs,
                style,
                fields,
            }
        })
        .collect()
}

fn struct_from_ast<'a>(
    cx: &Errors,
    fields: &'a Fields,
    container_default: &attr::Default,
) -> (Style, Vec<Field<'a>>) {
    match fields {
        Fields::Named(fields) => (
            Style::Struct,
            fields_from_ast(cx, fields, container_default),
        ),
        Fields::Unnamed(fields) if fields.len() == 1 => (
            Style::Newtype,
            fields_from_ast(cx, fields, container_default),
        ),
        Fields::Unnamed(fields) => (Style::Tuple, fields_from_ast(cx, fields, container_default)),
        Fields::Unit => (Style::Unit, Vec::new()),
    }
}

fn fields_from_ast<'a>(
    cx: &Errors,
    fields: &'a [aterm_macro_support::Field],
    container_default: &attr::Default,
) -> Vec<Field<'a>> {
    fields
        .iter()
        .enumerate()
        .map(|(i, field)| {
            let ty_tree = parse_type(&field.ty);
            Field {
                member: match &field.ident {
                    Some(ident) => Member::Named(ident.clone()),
                    None => Member::Unnamed(u32::try_from(i).unwrap_or(u32::MAX)),
                },
                attrs: attr::Field::from_ast(cx, i, field, &ty_tree, container_default),
                ty: field.ty_stream(),
                ty_tree,
                original: field,
            }
        })
        .collect()
}

/// Cross-cutting checks that require looking at more than a single attrs object.
fn check(cx: &Errors, cont: &mut Container, derive: Derive) {
    check_default_on_tuple(cx, cont);
    check_flatten(cx, cont);
    check_internal_tag_field_name_conflict(cx, cont);
    check_transparent(cx, cont, derive);
}

fn check_default_on_tuple(cx: &Errors, cont: &Container) {
    if let attr::Default::None = cont.attrs.default()
        && let Data::Struct(Style::Tuple, fields) = &cont.data
    {
        let mut first_default_index = None;
        for (i, field) in fields.iter().enumerate() {
            // Skipped fields automatically get the #[serde(default)]
            if field.attrs.skip_deserializing() {
                continue;
            }
            if let attr::Default::None = field.attrs.default() {
                if let Some(first) = first_default_index {
                    let ty: Vec<TokenTree> = field.ty.clone().into_iter().collect();
                    cx.push(aterm_macro_support::Error::spanned_by(
                        &ty,
                        format!("field must have #[serde(default)] because previous field {first} has #[serde(default)]"),
                    ));
                }
                continue;
            }
            if first_default_index.is_none() {
                first_default_index = Some(i);
            }
        }
    }
}

fn check_flatten(cx: &Errors, cont: &Container) {
    match &cont.data {
        Data::Enum(variants) => {
            for variant in variants {
                for field in &variant.fields {
                    check_flatten_field(cx, variant.style, field);
                }
            }
        }
        Data::Struct(style, fields) => {
            for field in fields {
                check_flatten_field(cx, *style, field);
            }
        }
    }
}

fn check_flatten_field(cx: &Errors, style: Style, field: &Field) {
    if !field.attrs.flatten() {
        return;
    }
    let span = (field.original.first_span, field.original.last_span);
    match style {
        Style::Tuple => {
            cx.spanned(
                span.0,
                span.1,
                "#[serde(flatten)] cannot be used on tuple structs",
            );
        }
        Style::Newtype => {
            cx.spanned(
                span.0,
                span.1,
                "#[serde(flatten)] cannot be used on newtype structs",
            );
        }
        _ => {}
    }
}

fn check_internal_tag_field_name_conflict(cx: &Errors, cont: &Container) {
    let Data::Enum(variants) = &cont.data else {
        return;
    };
    let attr::TagType::Internal { tag } = cont.attrs.tag() else {
        return;
    };
    let tag = tag.as_str();

    for variant in variants {
        match variant.style {
            Style::Struct => {
                for field in &variant.fields {
                    let check_ser = !field.attrs.skip_serializing();
                    let check_de = !field.attrs.skip_deserializing();
                    let name = field.attrs.name();
                    let ser_name = name.serialize_name();

                    if check_ser && ser_name.value == tag {
                        cont.error(
                            cx,
                            format!("variant field name `{tag}` conflicts with internal tag"),
                        );
                        return;
                    }

                    for de_name in field.attrs.aliases() {
                        if check_de && de_name.value == tag {
                            cont.error(
                                cx,
                                format!("variant field name `{tag}` conflicts with internal tag"),
                            );
                            return;
                        }
                    }
                }
            }
            Style::Unit | Style::Newtype | Style::Tuple => {}
        }
    }
}

fn check_transparent(cx: &Errors, cont: &mut Container, derive: Derive) {
    if !cont.attrs.transparent() {
        return;
    }
    let first = cont.original.first_span;
    let last = cont.original.last_span;

    let fields = match &mut cont.data {
        Data::Enum(_) => {
            cx.spanned(
                first,
                last,
                "#[serde(transparent)] is not allowed on an enum",
            );
            return;
        }
        Data::Struct(Style::Unit, _) => {
            cx.spanned(
                first,
                last,
                "#[serde(transparent)] is not allowed on a unit struct",
            );
            return;
        }
        Data::Struct(_, fields) => fields,
    };

    let mut transparent_field = None;

    for field in fields {
        if allow_transparent(field, derive) {
            if transparent_field.is_some() {
                cx.spanned(
                    first,
                    last,
                    "#[serde(transparent)] requires struct to have at most one transparent field",
                );
                return;
            }
            transparent_field = Some(field);
        }
    }

    match transparent_field {
        Some(transparent_field) => transparent_field.attrs.mark_transparent(),
        None => match derive {
            Derive::Serialize => {
                cx.spanned(
                    first,
                    last,
                    "#[serde(transparent)] requires at least one field that is not skipped",
                );
            }
            Derive::Deserialize => {
                cx.spanned(
                    first,
                    last,
                    "#[serde(transparent)] requires at least one field that is neither skipped nor has a default",
                );
            }
        },
    }
}

fn allow_transparent(field: &Field, derive: Derive) -> bool {
    if let Ty::Path(ty) = attr::ungroup(&field.ty_tree)
        && let Some(seg) = ty.segments.last()
        && seg.ident.to_string() == "PhantomData"
    {
        return false;
    }

    match derive {
        Derive::Serialize => !field.attrs.skip_serializing(),
        Derive::Deserialize => !field.attrs.skip_deserializing() && field.attrs.default().is_none(),
    }
}

/// The input with `Self` in its field types and generics replaced by the type itself.
pub fn replace_receiver(input: &DeriveInput) -> DeriveInput {
    let mut self_ty: Vec<TokenTree> = vec![TokenTree::Ident(input.ident.clone())];
    self_ty.extend(input.generics.ty_generics());
    let mut out = input.clone();
    let replace = |tokens: TokenStream| -> TokenStream {
        replace_self(tokens.into_iter().collect(), &self_ty)
            .into_iter()
            .collect()
    };
    for param in &mut out.generics.params {
        match param {
            GenericParam::Type {
                bounds, default, ..
            } => {
                for bound in bounds.iter_mut() {
                    *bound = replace(bound.clone());
                }
                if let Some(default) = default {
                    *default = replace(default.clone());
                }
            }
            GenericParam::Const { ty, .. } => *ty = replace(ty.clone()),
            GenericParam::Lifetime { .. } => {}
        }
    }
    for predicate in &mut out.generics.where_predicates {
        *predicate = replace(predicate.clone());
    }
    let fix_fields = |fields: &mut Fields| match fields {
        Fields::Named(list) | Fields::Unnamed(list) => {
            for field in list {
                field.ty = replace_self(std::mem::take(&mut field.ty), &self_ty);
            }
        }
        Fields::Unit => {}
    };
    match &mut out.data {
        InputData::Struct(fields) => fix_fields(fields),
        InputData::Enum(variants) => {
            for variant in variants {
                fix_fields(&mut variant.fields);
            }
        }
        InputData::Union(_) => {}
    }
    out
}

fn replace_self(tokens: Vec<TokenTree>, self_ty: &[TokenTree]) -> Vec<TokenTree> {
    let mut out = Vec::with_capacity(tokens.len());
    let mut iter = tokens.into_iter().peekable();
    while let Some(tt) = iter.next() {
        match tt {
            TokenTree::Ident(ident) if ident.to_string() == "Self" => {
                let span = ident.span();
                let respanned: Vec<TokenTree> =
                    aterm_macro_support::respan(self_ty.iter().cloned().collect(), span)
                        .into_iter()
                        .collect();
                let path_follows = matches!(iter.peek(), Some(TokenTree::Punct(p)) if p.as_char() == ':' && p.spacing() == Spacing::Joint);
                if path_follows {
                    let mut lt = Punct::new('<', Spacing::Alone);
                    lt.set_span(span);
                    let mut gt = Punct::new('>', Spacing::Alone);
                    gt.set_span(span);
                    out.push(TokenTree::Punct(lt));
                    out.extend(respanned);
                    out.push(TokenTree::Punct(gt));
                } else {
                    out.extend(respanned);
                }
            }
            TokenTree::Group(group) if matches!(out.as_slice(), [.., TokenTree::Ident(_), TokenTree::Punct(bang)] if bang.as_char() == '!') =>
            {
                out.push(TokenTree::Group(group));
            }
            TokenTree::Group(group) => {
                let inner = replace_self(group.stream().into_iter().collect(), self_ty);
                let mut replaced =
                    proc_macro::Group::new(group.delimiter(), inner.into_iter().collect());
                replaced.set_span(group.span());
                out.push(TokenTree::Group(replaced));
            }
            other => out.push(other),
        }
    }
    out
}
