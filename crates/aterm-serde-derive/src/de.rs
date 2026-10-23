// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
// Derived from serde_derive 1.0.228 src/de.rs (see ../LICENSE-MIT and ../LICENSE-APACHE).

use std::collections::{BTreeMap, BTreeSet};

use aterm_macro_support::{
    DeriveInput, Errors, GenericParam, Generics, Lifetime, ToTokens, q, q_spanned,
};
use proc_macro::{Ident, Literal, Span, TokenStream, TokenTree};

use crate::ast::{Container, Data, Derive, Field, Style, Variant};
use crate::attr::{self, Name};
use crate::bound;
use crate::fragment::{Expr, Fragment, Stmts};

mod enum_;
mod enum_externally;
mod enum_internally;
mod enum_untagged;
mod identifier;
mod struct_;
mod tuple;
mod unit;

pub fn expand_derive_deserialize(input: &DeriveInput) -> Result<TokenStream, TokenStream> {
    let cx = Errors::new();
    let Some(cont) = Container::from_ast(&cx, input, Derive::Deserialize) else {
        return Err(cx.finish().err().unwrap_or_default());
    };
    precondition(&cx, &cont);
    cx.finish()?;

    let cx = Errors::new();
    let params = Parameters::new(&cx, &cont);
    cx.finish()?;

    let ident = &cont.ident;
    let de_impl_generics = params.de_impl_generics();
    let ty_generics = params.generics.ty_generics();
    let where_clause = params.generics.where_clause();
    let body = Stmts(deserialize_body(&cont, &params));
    let delife = params.borrowed.de_lifetime();
    let allow_deprecated = crate::allow_deprecated(input);

    let impl_block = q!(
        "#[automatically_derived]
        #allow_deprecated
        impl #de_impl_generics _serde::Deserialize<#delife> for #ident #ty_generics #where_clause {
            fn deserialize<__D>(__deserializer: __D) -> _serde::__private::Result<Self, __D::Error>
            where
                __D: _serde::Deserializer<#delife>,
            {
                #body
            }
        }",
        allow_deprecated,
        de_impl_generics,
        delife,
        ident,
        ty_generics,
        where_clause,
        body,
    );

    Ok(crate::wrap_in_const(&impl_block))
}

fn precondition(cx: &Errors, cont: &Container) {
    precondition_sized(cx, cont);
    precondition_no_de_lifetime(cx, cont);
}

fn precondition_sized(cx: &Errors, cont: &Container) {
    if let Data::Struct(_, fields) = &cont.data
        && let Some(last) = fields.last()
        && let aterm_macro_support::Ty::Slice(_) = attr::ungroup(&last.ty_tree)
    {
        cont.error(cx, "cannot deserialize a dynamically sized struct");
    }
}

fn precondition_no_de_lifetime(cx: &Errors, cont: &Container) {
    if let BorrowedLifetimes::Borrowed(_) = borrowed_lifetimes(cont) {
        for lifetime in cont.generics.lifetimes() {
            if lifetime.name() == "'de" {
                cx.at(
                    lifetime.ident.span(),
                    "cannot deserialize when there is a lifetime parameter called 'de",
                );
                return;
            }
        }
    }
}

pub(crate) struct Parameters {
    /// The type the impl is for (does not include generic parameters).
    this_type: Ident,

    /// Generics including any explicit and inferred bounds for the impl.
    generics: Generics,

    /// Lifetimes borrowed from the deserializer.
    borrowed: BorrowedLifetimes,
}

impl Parameters {
    fn new(cx: &Errors, cont: &Container) -> Self {
        let borrowed = borrowed_lifetimes(cont);
        let generics = build_generics(cx, cont, &borrowed);
        Parameters {
            this_type: cont.ident.clone(),
            generics,
            borrowed,
        }
    }

    /// Type name for error messages and `Deserializer` arguments.
    fn type_name(&self) -> String {
        self.this_type.to_string()
    }

    /// The data structure's generics for its `Deserialize` impl.
    fn de_impl_generics(&self) -> TokenStream {
        let mut generics = self.generics.clone();
        if let Some(de_lifetime) = self.borrowed.de_lifetime_param() {
            generics.params.insert(0, de_lifetime);
        }
        generics.impl_generics()
    }

    /// The type arguments of the visitor types, `'de` in front.
    fn de_ty_generics(&self) -> TokenStream {
        let mut generics = self.generics.clone();
        if self.borrowed.de_lifetime_param().is_some() {
            generics.params.insert(
                0,
                GenericParam::Lifetime {
                    attrs: Vec::new(),
                    lifetime: Lifetime::new("de", Span::call_site()),
                    bounds: Vec::new(),
                },
            );
        }
        generics.ty_generics()
    }
}

fn build_generics(cx: &Errors, cont: &Container, borrowed: &BorrowedLifetimes) -> Generics {
    let generics = match *cont.attrs.default() {
        attr::Default::Default => {
            bound::with_self_bound(cx, cont, cont.generics, &q!("_serde::__private::Default"))
        }
        attr::Default::None | attr::Default::Path(_) => cont.generics.clone(),
    };

    let delife = borrowed.de_lifetime();
    let generics = bound::with_bound(
        cont,
        &generics,
        needs_deserialize_bound,
        &q!("_serde::Deserialize<#delife>", delife),
    );

    bound::with_bound(
        cont,
        &generics,
        requires_default,
        &q!("_serde::__private::Default"),
    )
}

const fn needs_deserialize_bound(field: &attr::Field) -> bool {
    !field.skip_deserializing() && field.deserialize_with().is_none()
}

const fn requires_default(field: &attr::Field) -> bool {
    matches!(field.default(), attr::Default::Default)
}

pub(crate) enum BorrowedLifetimes {
    Borrowed(BTreeMap<String, Lifetime>),
    Static,
}

impl BorrowedLifetimes {
    fn de_lifetime(&self) -> TokenStream {
        match *self {
            BorrowedLifetimes::Borrowed(_) => Lifetime::new("de", Span::call_site()),
            BorrowedLifetimes::Static => Lifetime::new("static", Span::call_site()),
        }
        .to_token_stream()
    }

    fn de_lifetime_param(&self) -> Option<GenericParam> {
        match self {
            BorrowedLifetimes::Borrowed(bounds) => Some(GenericParam::Lifetime {
                attrs: Vec::new(),
                lifetime: Lifetime::new("de", Span::call_site()),
                bounds: bounds.values().cloned().collect(),
            }),
            BorrowedLifetimes::Static => None,
        }
    }
}

fn borrowed_lifetimes(cont: &Container) -> BorrowedLifetimes {
    let mut lifetimes = BTreeMap::new();
    for field in cont.data.all_fields() {
        if !field.attrs.skip_deserializing() {
            for (name, lifetime) in field.attrs.borrowed_lifetimes() {
                lifetimes.insert(name.clone(), lifetime.clone());
            }
        }
    }
    if lifetimes.contains_key("'static") {
        BorrowedLifetimes::Static
    } else {
        BorrowedLifetimes::Borrowed(lifetimes)
    }
}

fn deserialize_body(cont: &Container, params: &Parameters) -> Fragment {
    if cont.attrs.transparent() {
        deserialize_transparent(cont, params)
    } else {
        match &cont.data {
            Data::Enum(variants) => enum_::deserialize(params, variants, &cont.attrs),
            Data::Struct(Style::Struct, fields) => {
                struct_::deserialize(params, fields, &cont.attrs, &StructForm::Struct)
            }
            Data::Struct(Style::Tuple | Style::Newtype, fields) => {
                tuple::deserialize(params, fields, &cont.attrs, &TupleForm::Tuple)
            }
            Data::Struct(Style::Unit, _) => unit::deserialize(params, &cont.attrs),
        }
    }
}

/// Generates `Deserialize::deserialize` body for a type with `#[serde(transparent)]`.
fn deserialize_transparent(cont: &Container, params: &Parameters) -> Fragment {
    let this_value = &params.this_type;
    let (Data::Struct(_, fields), Some(transparent_field)) = (
        &cont.data,
        cont.data.all_fields().find(|f| f.attrs.transparent()),
    ) else {
        return quote_expr!("_serde::__private::Err(_serde::de::Error::custom(\"transparent\"))");
    };

    let path = match transparent_field.attrs.deserialize_with() {
        Some(path) => path.clone(),
        None => q_spanned!(transparent_field.span() => "_serde::Deserialize::deserialize"),
    };

    let assign: Vec<TokenStream> = fields
        .iter()
        .map(|field| {
            let member = &field.member;
            if std::ptr::eq(field, transparent_field) {
                q!("#member: __transparent", member)
            } else {
                let value = match field.attrs.default() {
                    attr::Default::Default => q!("_serde::__private::Default::default()"),
                    attr::Default::Path(path) => q_spanned!(span_of(path) => "#path()", path),
                    attr::Default::None => q!("_serde::__private::PhantomData"),
                };
                q!("#member: #value", member, value)
            }
        })
        .collect();
    let assign = aterm_macro_support::join(&assign, ',', false);

    quote_block!(
        "_serde::__private::Result::map(
            #path(__deserializer),
            |__transparent| #this_value { #assign })",
        path,
        this_value,
        assign,
    )
}

pub(crate) enum TupleForm<'a> {
    Tuple,
    /// Contains a variant name.
    ExternallyTagged(&'a Ident),
    /// Contains a variant name.
    Untagged(&'a Ident),
}

fn deserialize_seq(
    type_path: &TokenStream,
    params: &Parameters,
    fields: &[Field],
    is_struct: bool,
    cattrs: &attr::Container,
    expecting: &str,
) -> Fragment {
    let vars: Vec<Ident> = (0..fields.len()).map(field_i).collect();

    let deserialized_count = fields
        .iter()
        .filter(|field| !field.attrs.skip_deserializing())
        .count();
    let expecting = if deserialized_count == 1 {
        format!("{expecting} with 1 element")
    } else {
        format!("{expecting} with {deserialized_count} elements")
    };

    let mut index_in_seq = 0_usize;
    let let_values: Vec<TokenStream> = vars
        .iter()
        .zip(fields)
        .map(|(var, field)| {
            if field.attrs.skip_deserializing() {
                let default = Expr(expr_is_missing(field, cattrs));
                q!("let #var = #default;", var, default)
            } else {
                let visit = match field.attrs.deserialize_with() {
                    None => {
                        let field_ty = &field.ty;
                        let func = q_spanned!(field.span() => "_serde::de::SeqAccess::next_element::<#field_ty>", field_ty);
                        q!("#func(&mut __seq)?", func)
                    }
                    Some(path) => {
                        let (wrapper, wrapper_ty) = wrap_deserialize_field_with(params, &field.ty, path);
                        q!(
                            "{
                                #wrapper
                                _serde::__private::Option::map(
                                    _serde::de::SeqAccess::next_element::<#wrapper_ty>(&mut __seq)?,
                                    |__wrap| __wrap.value)
                            }",
                            wrapper,
                            wrapper_ty
                        )
                    }
                };
                let value_if_none = expr_is_missing_seq(index_in_seq, field, cattrs, &expecting);
                let assign = q!(
                    "let #var = match #visit {
                        _serde::__private::Some(__value) => __value,
                        _serde::__private::None => #value_if_none,
                    };",
                    var,
                    visit,
                    value_if_none
                );
                index_in_seq += 1;
                assign
            }
        })
        .collect();

    let result = if is_struct {
        let assigns: Vec<TokenStream> = fields
            .iter()
            .zip(&vars)
            .map(|(f, var)| {
                let member = &f.member;
                q!("#member: #var", member, var)
            })
            .collect();
        let assigns = aterm_macro_support::join(&assigns, ',', false);
        q!("#type_path { #assigns }", type_path, assigns)
    } else {
        let vars = aterm_macro_support::join(&vars, ',', false);
        q!("#type_path ( #vars )", type_path, vars)
    };

    let let_default = let_default(cattrs);

    quote_block!(
        "#let_default
        #let_values
        _serde::__private::Ok(#result)",
        let_default,
        let_values,
        result,
    )
}

/// `let __default: Self::Value = ...;` under a container `default`.
fn let_default(cattrs: &attr::Container) -> Option<TokenStream> {
    match cattrs.default() {
        attr::Default::Default => Some(q!(
            "let __default: Self::Value = _serde::__private::Default::default();"
        )),
        attr::Default::Path(path) => Some(q_spanned!(span_of(path) =>
            "let __default: Self::Value = #path();", path)),
        attr::Default::None => {
            // Omit `__default` when no container default is configured.
            None
        }
    }
}

pub(crate) enum StructForm<'a> {
    Struct,
    /// Contains a variant name.
    ExternallyTagged(&'a Ident),
    /// Contains a variant name.
    InternallyTagged(&'a Ident),
    /// Contains a variant name.
    Untagged(&'a Ident),
}

pub(crate) struct FieldWithAliases<'a> {
    ident: Ident,
    aliases: &'a BTreeSet<Name>,
}

fn field_i(i: usize) -> Ident {
    Ident::new(&format!("__field{i}"), Span::call_site())
}

/// The span of a path's first token.
fn span_of(tokens: &TokenStream) -> Span {
    tokens
        .clone()
        .into_iter()
        .next()
        .map_or_else(Span::call_site, |tt| tt.span())
}

/// Wraps `#[serde(deserialize_with = "...")]` in a trait impl to isolate `Deserialize` state.
fn wrap_deserialize_with(
    params: &Parameters,
    value_ty: &TokenStream,
    deserialize_with: &TokenStream,
) -> (TokenStream, TokenStream) {
    let this_type = &params.this_type;
    let de_impl_generics = params.de_impl_generics();
    let de_ty_generics = params.de_ty_generics();
    let ty_generics = params.generics.ty_generics();
    let where_clause = params.generics.where_clause();
    let delife = params.borrowed.de_lifetime();
    let deserializer_var = q!("__deserializer");

    let value = q_spanned!(span_of(deserialize_with) =>
        "#deserialize_with(#deserializer_var)?", deserialize_with, deserializer_var);
    let wrapper = q!(
        "#[doc(hidden)]
        struct __DeserializeWith #de_impl_generics #where_clause {
            value: #value_ty,
            phantom: _serde::__private::PhantomData<#this_type #ty_generics>,
            lifetime: _serde::__private::PhantomData<&#delife ()>,
        }

        #[automatically_derived]
        impl #de_impl_generics _serde::Deserialize<#delife> for __DeserializeWith #de_ty_generics #where_clause {
            fn deserialize<__D>(#deserializer_var: __D) -> _serde::__private::Result<Self, __D::Error>
            where
                __D: _serde::Deserializer<#delife>,
            {
                _serde::__private::Ok(__DeserializeWith {
                    value: #value,
                    phantom: _serde::__private::PhantomData,
                    lifetime: _serde::__private::PhantomData,
                })
            }
        }",
        de_impl_generics,
        where_clause,
        value_ty,
        this_type,
        ty_generics,
        delife,
        de_ty_generics,
        deserializer_var,
        value,
    );

    let wrapper_ty = q!("__DeserializeWith #de_ty_generics", de_ty_generics);

    (wrapper, wrapper_ty)
}

fn wrap_deserialize_field_with(
    params: &Parameters,
    field_ty: &TokenStream,
    deserialize_with: &TokenStream,
) -> (TokenStream, TokenStream) {
    wrap_deserialize_with(params, field_ty, deserialize_with)
}

fn expr_is_missing(field: &Field, cattrs: &attr::Container) -> Fragment {
    match field.attrs.default() {
        attr::Default::Default => {
            let func = q_spanned!(field.span() => "_serde::__private::Default::default");
            return quote_expr!("#func()", func);
        }
        attr::Default::Path(path) => {
            return Fragment::Expr(q_spanned!(span_of(path) => "#path()", path));
        }
        attr::Default::None => { /* below */ }
    }

    match *cattrs.default() {
        attr::Default::Default | attr::Default::Path(_) => {
            let member = &field.member;
            return quote_expr!("__default.#member", member);
        }
        attr::Default::None => { /* below */ }
    }

    let name = field.attrs.name().deserialize_name();
    match field.attrs.deserialize_with() {
        None => {
            let func = q_spanned!(field.span() => "_serde::__private::de::missing_field");
            quote_expr!("#func(#name)?", func, name)
        }
        Some(_) => quote_expr!(
            "return _serde::__private::Err(<__A::Error as _serde::de::Error>::missing_field(#name))",
            name
        ),
    }
}

fn expr_is_missing_seq(
    index: usize,
    field: &Field,
    cattrs: &attr::Container,
    expecting: &str,
) -> TokenStream {
    match field.attrs.default() {
        attr::Default::Default => {
            return q_spanned!(field.span() => "_serde::__private::Default::default()");
        }
        attr::Default::Path(path) => {
            return q_spanned!(span_of(path) => "#path()", path);
        }
        attr::Default::None => { /* below */ }
    }

    match *cattrs.default() {
        attr::Default::Default | attr::Default::Path(_) => {
            let member = &field.member;
            q!("__default.#member", member)
        }
        attr::Default::None => q!(
            "return _serde::__private::Err(_serde::de::Error::invalid_length(#index, &#expecting))",
            index = Literal::usize_suffixed(index),
            expecting = Literal::string(expecting),
        ),
    }
}

fn effective_style(variant: &Variant) -> Style {
    match variant.style {
        Style::Newtype if variant.fields[0].attrs.skip_deserializing() => Style::Unit,
        other => other,
    }
}

/// True if there is any field with a `#[serde(flatten)]` attribute.
fn has_flatten(fields: &[Field]) -> bool {
    fields
        .iter()
        .any(|field| field.attrs.flatten() && !field.attrs.skip_deserializing())
}

/// `__Visitor { marker: PhantomData::<T>, lifetime: PhantomData }`.
fn visitor_expr(params: &Parameters) -> TokenStream {
    let this_type = &params.this_type;
    let ty_generics = params.generics.ty_generics();
    q!(
        "__Visitor {
            marker: _serde::__private::PhantomData::<#this_type #ty_generics>,
            lifetime: _serde::__private::PhantomData,
        }",
        this_type,
        ty_generics
    )
}

/// The `__Visitor` struct every visitor-based body declares.
fn visitor_struct(params: &Parameters) -> TokenStream {
    let this_type = &params.this_type;
    let de_impl_generics = params.de_impl_generics();
    let ty_generics = params.generics.ty_generics();
    let where_clause = params.generics.where_clause();
    let delife = params.borrowed.de_lifetime();
    q!(
        "#[doc(hidden)]
        struct __Visitor #de_impl_generics #where_clause {
            marker: _serde::__private::PhantomData<#this_type #ty_generics>,
            lifetime: _serde::__private::PhantomData<&#delife ()>,
        }",
        de_impl_generics,
        where_clause,
        this_type,
        ty_generics,
        delife
    )
}

/// `impl Visitor<'de> for __Visitor` around `body` (its items after `expecting`).
fn visitor_impl(params: &Parameters, expecting: &str, body: &dyn ToTokens) -> TokenStream {
    let this_type = &params.this_type;
    let de_impl_generics = params.de_impl_generics();
    let de_ty_generics = params.de_ty_generics();
    let ty_generics = params.generics.ty_generics();
    let where_clause = params.generics.where_clause();
    let delife = params.borrowed.de_lifetime();
    let expecting = Literal::string(expecting);
    let body: Vec<TokenTree> = {
        let mut out = Vec::new();
        body.to_tokens(&mut out);
        out
    };
    q!(
        "#[automatically_derived]
        impl #de_impl_generics _serde::de::Visitor<#delife> for __Visitor #de_ty_generics #where_clause {
            type Value = #this_type #ty_generics;

            fn expecting(&self, __formatter: &mut _serde::__private::Formatter) -> _serde::__private::fmt::Result {
                _serde::__private::Formatter::write_str(__formatter, #expecting)
            }

            #body
        }",
        de_impl_generics,
        delife,
        de_ty_generics,
        where_clause,
        this_type,
        ty_generics,
        expecting,
        body,
    )
}
