// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Derive macro for `aterm_error::Error`.
//!
//! Generates `Display` and `std::error::Error` implementations from enum
//! variants annotated with `#[error("...")]`.
//!
//! ## Supported attributes
//!
//! - `#[error("literal format string")]` — Display format with named field interpolation
//! - `#[error("fmt {}", expr1, expr2)]` — Display format with explicit arguments
//! - `#[error(transparent)]` — delegate Display and source() to the inner field
//! - `#[from]` on a field — generate `From<T>` impl for that variant
//! - `#[source]` on a field — report that field as the error source

#![deny(clippy::all)]

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{
    Attribute, Data, DataStruct, DeriveInput, Expr, ExprLit, Fields, Lit, Meta, MetaNameValue,
    Variant, parse_macro_input,
};

/// Derive `Display` and `std::error::Error` for an enum.
///
/// Supports `#[error("...")]`, `#[error(transparent)]`, `#[from]`, and `#[source]`.
#[proc_macro_derive(Error, attributes(error, from, source))]
pub fn derive_error(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match derive_error_impl(input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

fn derive_error_impl(input: DeriveInput) -> syn::Result<TokenStream2> {
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    match &input.data {
        Data::Struct(data) => {
            return derive_struct_error(
                &input,
                name,
                &impl_generics,
                &ty_generics,
                where_clause,
                data,
            );
        }
        Data::Enum(_) => {} // handled below
        Data::Union(_) => {
            return Err(syn::Error::new_spanned(
                &input.ident,
                "Error derive does not support unions",
            ));
        }
    }

    let data = match &input.data {
        Data::Enum(data) => data,
        _ => unreachable!(),
    };

    let mut display_arms = Vec::new();
    let mut source_arms = Vec::new();
    let mut from_impls = Vec::new();

    for variant in &data.variants {
        let error_attr = find_error_attr(variant)?;

        match &error_attr {
            ErrorAttr::Format { fmt_str, fmt_args } => {
                let (pattern, display_impl) = build_format_arm(name, variant, fmt_str, fmt_args)?;
                display_arms.push(quote! { #pattern => #display_impl, });

                let source_arm = build_source_arm(name, variant)?;
                source_arms.push(source_arm);
            }
            ErrorAttr::Transparent => {
                let (pattern, field_access) = build_transparent_pattern(name, variant)?;
                display_arms.push(quote! {
                    #pattern => ::core::fmt::Display::fmt(#field_access, f),
                });
                source_arms.push(quote! {
                    #pattern => ::core::option::Option::Some(#field_access),
                });
            }
        }

        // Generate From impls for #[from] fields
        for from_impl in build_from_impls(name, variant, &input.generics)? {
            from_impls.push(from_impl);
        }
    }

    // If no variants have a source, use a catch-all returning None
    let source_body = if source_arms.is_empty() {
        quote! { ::core::option::Option::None }
    } else {
        quote! {
            match self {
                #(#source_arms)*
                #[allow(unreachable_patterns)]
                _ => ::core::option::Option::None,
            }
        }
    };

    let expanded = quote! {
        impl #impl_generics ::core::fmt::Display for #name #ty_generics #where_clause {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                match self {
                    #(#display_arms)*
                }
            }
        }

        impl #impl_generics ::std::error::Error for #name #ty_generics #where_clause {
            fn source(&self) -> ::core::option::Option<&(dyn ::std::error::Error + 'static)> {
                #source_body
            }
        }

        #(#from_impls)*
    };

    Ok(expanded)
}

/// Derive `Display` and `Error` for a struct type.
///
/// Supports:
/// - Tuple structs: `#[error("message {0}")]` or `#[error("msg {}", .0.display())]`
/// - Named structs: `#[error("message {field}")]` or `#[error("msg {}", field.0)]`
#[allow(clippy::too_many_arguments)]
fn derive_struct_error(
    input: &DeriveInput,
    name: &syn::Ident,
    impl_generics: &syn::ImplGenerics<'_>,
    ty_generics: &syn::TypeGenerics<'_>,
    where_clause: Option<&syn::WhereClause>,
    data: &DataStruct,
) -> syn::Result<TokenStream2> {
    // Find #[error("...")] on the struct itself
    let error_attr = find_struct_error_attr(&input.attrs)?;

    let (display_body, source_body) = match &error_attr {
        ErrorAttr::Format { fmt_str, fmt_args } => {
            let display = build_struct_display(name, &data.fields, fmt_str, fmt_args)?;
            let source = build_struct_source(&data.fields)?;
            (display, source)
        }
        ErrorAttr::Transparent => {
            let (access, _) = single_struct_field(&data.fields)?;
            let display = quote! { ::core::fmt::Display::fmt(&#access, f) };
            let source =
                quote! { ::core::option::Option::Some(&#access as &dyn ::std::error::Error) };
            (display, source)
        }
    };

    // Generate From impls for #[from] fields
    let from_impls = build_struct_from_impls(name, &data.fields, &input.generics)?;

    let expanded = quote! {
        impl #impl_generics ::core::fmt::Display for #name #ty_generics #where_clause {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                #display_body
            }
        }

        impl #impl_generics ::std::error::Error for #name #ty_generics #where_clause {
            fn source(&self) -> ::core::option::Option<&(dyn ::std::error::Error + 'static)> {
                #source_body
            }
        }

        #(#from_impls)*
    };

    Ok(expanded)
}

/// Find `#[error("...")]` attribute on a struct (not on a variant).
fn find_struct_error_attr(attrs: &[Attribute]) -> syn::Result<ErrorAttr> {
    for attr in attrs {
        if !attr.path().is_ident("error") {
            continue;
        }

        if let Meta::List(meta_list) = &attr.meta {
            let tokens_str = meta_list.tokens.to_string();
            if tokens_str.trim() == "transparent" {
                return Ok(ErrorAttr::Transparent);
            }
            let parsed: ErrorFormatArgs = syn::parse2(meta_list.tokens.clone())?;
            return Ok(ErrorAttr::Format {
                fmt_str: parsed.fmt_str.value(),
                fmt_args: parsed.args,
            });
        }
    }

    Err(syn::Error::new(
        proc_macro2::Span::call_site(),
        "missing #[error(\"...\")] attribute on struct",
    ))
}

/// Build the Display body for a struct with a format string.
fn build_struct_display(
    _name: &syn::Ident,
    fields: &Fields,
    fmt_str: &str,
    fmt_args: &[TokenStream2],
) -> syn::Result<TokenStream2> {
    if !fmt_args.is_empty() {
        // Explicit args: rewrite `.N` to `self.N` for tuple, `.field` to `self.field` for named
        let rewritten_args: Vec<TokenStream2> = fmt_args
            .iter()
            .map(|arg| rewrite_struct_field_refs(arg, fields))
            .collect();
        return Ok(quote! { ::core::write!(f, #fmt_str, #(#rewritten_args),*) });
    }

    match fields {
        Fields::Unit => Ok(write_literal(fmt_str)),
        Fields::Unnamed(uf) => {
            let mut rewritten = fmt_str.to_string();
            // Replace {0} -> {__0}, {1} -> {__1}, bare {} -> {__0} for single-field
            for i in 0..uf.unnamed.len() {
                let positional = format!("{{{i}}}");
                let named_ref = format!("{{__{i}}}");
                rewritten = rewritten.replace(&positional, &named_ref);
            }
            if uf.unnamed.len() == 1 && rewritten.contains("{}") {
                rewritten = rewritten.replace("{}", "{__0}");
            }
            if uf.unnamed.len() == 1 && rewritten.contains("{:?}") {
                rewritten = rewritten.replace("{:?}", "{__0:?}");
            }
            // Replace {0:spec} patterns
            for i in 0..uf.unnamed.len() {
                let prefix = format!("{{{i}:");
                while let Some(start) = rewritten.find(&prefix) {
                    if let Some(end) = rewritten[start..].find('}') {
                        let spec = &rewritten[start + prefix.len()..start + end];
                        let replacement = format!("{{__{i}:{spec}}}");
                        rewritten = format!(
                            "{}{}{}",
                            &rewritten[..start],
                            replacement,
                            &rewritten[start + end + 1..]
                        );
                    } else {
                        break;
                    }
                }
            }
            // Build self.0, self.1, ... bindings
            let refs = format_referenced_names(&rewritten);
            let bindings: Vec<TokenStream2> = (0..uf.unnamed.len())
                .filter(|i| refs.contains(&format!("__{i}")))
                .map(|i| {
                    let ident = format_ident!("__{}", i);
                    let idx = syn::Index::from(i);
                    quote! { let #ident = &self.#idx; }
                })
                .collect();
            Ok(quote! {
                #(#bindings)*
                ::core::write!(f, #rewritten)
            })
        }
        Fields::Named(nf) => {
            let field_names: Vec<_> = nf
                .named
                .iter()
                .map(|f| f.ident.clone().expect("named field has ident"))
                .collect();
            let refs = format_referenced_names(fmt_str);
            let referenced: Vec<_> = field_names
                .iter()
                .filter(|ident| refs.contains(&ident.to_string()))
                .collect();
            if referenced.is_empty() {
                Ok(write_literal(fmt_str))
            } else {
                let bindings: Vec<TokenStream2> = referenced
                    .iter()
                    .map(|ident| quote! { let #ident = &self.#ident; })
                    .collect();
                Ok(quote! {
                    #(#bindings)*
                    ::core::write!(f, #fmt_str, #(#referenced = #referenced),*)
                })
            }
        }
    }
}

/// Build the source() body for a struct.
fn build_struct_source(fields: &Fields) -> syn::Result<TokenStream2> {
    match fields {
        Fields::Named(nf) => {
            for field in &nf.named {
                if has_attr(&field.attrs, "source") || has_attr(&field.attrs, "from") {
                    let name = field.ident.as_ref().expect("named field");
                    return Ok(quote! { ::core::option::Option::Some(&self.#name) });
                }
            }
        }
        Fields::Unnamed(uf) => {
            for (i, field) in uf.unnamed.iter().enumerate() {
                if has_attr(&field.attrs, "source") || has_attr(&field.attrs, "from") {
                    let idx = syn::Index::from(i);
                    return Ok(quote! { ::core::option::Option::Some(&self.#idx) });
                }
            }
        }
        Fields::Unit => {}
    }
    Ok(quote! { ::core::option::Option::None })
}

/// Get the single field accessor for a transparent struct.
fn single_struct_field(fields: &Fields) -> syn::Result<(TokenStream2, &syn::Field)> {
    match fields {
        Fields::Unnamed(uf) if uf.unnamed.len() == 1 => {
            let idx = syn::Index::from(0);
            Ok((quote! { self.#idx }, &uf.unnamed[0]))
        }
        Fields::Named(nf) if nf.named.len() == 1 => {
            let name = nf.named[0].ident.as_ref().expect("named field");
            Ok((quote! { self.#name }, &nf.named[0]))
        }
        _ => Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            "#[error(transparent)] on struct requires exactly one field",
        )),
    }
}

/// Generate `From<T>` impls for #[from] fields on structs.
fn build_struct_from_impls(
    struct_name: &syn::Ident,
    fields: &Fields,
    generics: &syn::Generics,
) -> syn::Result<Vec<TokenStream2>> {
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let mut impls = Vec::new();

    match fields {
        Fields::Unnamed(uf) => {
            for (i, field) in uf.unnamed.iter().enumerate() {
                if has_attr(&field.attrs, "from") {
                    let ty = &field.ty;
                    let construction = if uf.unnamed.len() == 1 {
                        quote! { #struct_name(value) }
                    } else {
                        let args: Vec<_> = (0..uf.unnamed.len())
                            .map(|j| {
                                if j == i {
                                    quote! { value }
                                } else {
                                    quote! { ::core::default::Default::default() }
                                }
                            })
                            .collect();
                        quote! { #struct_name(#(#args),*) }
                    };
                    impls.push(quote! {
                        impl #impl_generics ::core::convert::From<#ty> for #struct_name #ty_generics #where_clause {
                            fn from(value: #ty) -> Self {
                                #construction
                            }
                        }
                    });
                }
            }
        }
        Fields::Named(nf) => {
            for field in &nf.named {
                if has_attr(&field.attrs, "from") {
                    let ty = &field.ty;
                    let field_name = field.ident.as_ref().expect("named field");
                    let other_fields: Vec<_> = nf
                        .named
                        .iter()
                        .filter(|f| f.ident.as_ref() != Some(field_name))
                        .map(|f| {
                            let name = f.ident.as_ref().expect("named field");
                            quote! { #name: ::core::default::Default::default() }
                        })
                        .collect();
                    impls.push(quote! {
                        impl #impl_generics ::core::convert::From<#ty> for #struct_name #ty_generics #where_clause {
                            fn from(value: #ty) -> Self {
                                #struct_name {
                                    #field_name: value,
                                    #(#other_fields),*
                                }
                            }
                        }
                    });
                }
            }
        }
        Fields::Unit => {}
    }

    Ok(impls)
}

/// Rewrite field references in explicit format args for struct context.
/// `.0` -> `self.0`, `.field` -> `self.field`, bare idents stay as-is.
#[allow(clippy::collapsible_if)]
fn rewrite_struct_field_refs(tokens: &TokenStream2, fields: &Fields) -> TokenStream2 {
    let token_vec: Vec<proc_macro2::TokenTree> = tokens.clone().into_iter().collect();

    if token_vec.len() >= 2 {
        if let proc_macro2::TokenTree::Punct(p) = &token_vec[0] {
            if p.as_char() == '.' {
                match &token_vec[1] {
                    proc_macro2::TokenTree::Literal(lit) => {
                        // `.0` -> `self.0`
                        let lit_str = lit.to_string();
                        if lit_str.parse::<usize>().is_ok() {
                            let rest: TokenStream2 = token_vec[1..].iter().cloned().collect();
                            return quote! { self . #rest };
                        }
                    }
                    proc_macro2::TokenTree::Ident(ident) => {
                        // `.field` -> `self.field` if it's a known field
                        if let Fields::Named(nf) = fields {
                            let is_field = nf.named.iter().any(|f| f.ident.as_ref() == Some(ident));
                            if is_field {
                                let rest: TokenStream2 = token_vec[1..].iter().cloned().collect();
                                return quote! { self . #rest };
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    tokens.clone()
}

enum ErrorAttr {
    /// Format string with optional extra format arguments (thiserror-style).
    /// When `fmt_args` is non-empty, the format string uses `write!(f, fmt, args...)`.
    Format {
        fmt_str: String,
        fmt_args: Vec<TokenStream2>,
    },
    Transparent,
}

/// Parse a comma-separated list: `"format string", expr1, expr2, ...`
///
/// Format args are collected as raw token streams because thiserror-style
/// shorthand like `.0.display()` (leading dot = field access on self) is not
/// valid Rust and would fail `syn::Expr` parsing.
struct ErrorFormatArgs {
    fmt_str: syn::LitStr,
    args: Vec<TokenStream2>,
}

impl syn::parse::Parse for ErrorFormatArgs {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let fmt_str: syn::LitStr = input.parse()?;
        let mut args = Vec::new();
        while input.peek(syn::Token![,]) {
            let _comma: syn::Token![,] = input.parse()?;
            if input.is_empty() {
                break;
            }
            // Collect tokens until the next comma or end of stream.
            let mut tokens = Vec::new();
            while !input.is_empty() && !input.peek(syn::Token![,]) {
                tokens.push(input.parse::<proc_macro2::TokenTree>()?);
            }
            let ts: TokenStream2 = tokens.into_iter().collect();
            args.push(ts);
        }
        Ok(Self { fmt_str, args })
    }
}

fn find_error_attr(variant: &Variant) -> syn::Result<ErrorAttr> {
    for attr in &variant.attrs {
        if !attr.path().is_ident("error") {
            continue;
        }

        // #[error(transparent)]
        if let Meta::List(meta_list) = &attr.meta {
            let tokens_str = meta_list.tokens.to_string();
            if tokens_str.trim() == "transparent" {
                return Ok(ErrorAttr::Transparent);
            }
        }

        // #[error("format string")] or #[error("format string", arg1, arg2, ...)]
        match &attr.meta {
            Meta::List(meta_list) => {
                let parsed: ErrorFormatArgs = syn::parse2(meta_list.tokens.clone())?;
                return Ok(ErrorAttr::Format {
                    fmt_str: parsed.fmt_str.value(),
                    fmt_args: parsed.args,
                });
            }
            Meta::NameValue(MetaNameValue {
                value:
                    Expr::Lit(ExprLit {
                        lit: Lit::Str(lit_str),
                        ..
                    }),
                ..
            }) => {
                return Ok(ErrorAttr::Format {
                    fmt_str: lit_str.value(),
                    fmt_args: Vec::new(),
                });
            }
            _ => {
                return Err(syn::Error::new_spanned(
                    attr,
                    "expected #[error(\"...\") or #[error(transparent)]",
                ));
            }
        }
    }

    Err(syn::Error::new_spanned(
        &variant.ident,
        "missing #[error(...)] attribute on variant",
    ))
}

fn has_attr(attrs: &[Attribute], name: &str) -> bool {
    attrs.iter().any(|a| a.path().is_ident(name))
}

/// Build the Display match arm for a format-string variant.
fn build_format_arm(
    enum_name: &syn::Ident,
    variant: &Variant,
    fmt_str: &str,
    fmt_args: &[TokenStream2],
) -> syn::Result<(TokenStream2, TokenStream2)> {
    let variant_name = &variant.ident;

    // When explicit format args are provided (thiserror-style), use them directly.
    if !fmt_args.is_empty() {
        return build_explicit_args_arm(enum_name, variant, fmt_str, fmt_args);
    }

    match &variant.fields {
        Fields::Unit => {
            let pattern = quote! { #enum_name::#variant_name };
            let display = write_literal(fmt_str);
            Ok((pattern, display))
        }
        Fields::Unnamed(fields) => {
            let field_names: Vec<_> = (0..fields.unnamed.len())
                .map(|i| format_ident!("__field{}", i))
                .collect();
            let pattern = quote! { #enum_name::#variant_name(#(#field_names),*) };

            // Parse format string for positional references like {0}, {1} or bare {}
            let display = build_format_call(fmt_str, &field_names, &[])?;
            Ok((pattern, display))
        }
        Fields::Named(fields) => {
            let field_names: Vec<_> = fields
                .named
                .iter()
                .map(|f| f.ident.clone().expect("named field has ident"))
                .collect();

            // Only bind fields referenced in the format string to avoid unused variable warnings.
            let refs = format_referenced_names(fmt_str);
            let referenced: Vec<_> = field_names
                .iter()
                .filter(|ident| refs.contains(&ident.to_string()))
                .collect();

            let pattern = quote! { #enum_name::#variant_name { #(#referenced,)* .. } };

            let display = build_format_call(fmt_str, &[], &field_names)?;
            Ok((pattern, display))
        }
    }
}

/// Build the Display match arm when explicit format arguments are provided.
///
/// Handles thiserror-style syntax like:
/// - `#[error("msg {}", MAX_LINE_LENGTH)]` -- constant expression
/// - `#[error("msg {}", .0.display())]` -- method call on positional field
/// - `#[error("msg {}", expected.0)]` -- tuple field access on named field
///
/// The `.N` shorthand is rewritten to the corresponding field binding.
fn build_explicit_args_arm(
    enum_name: &syn::Ident,
    variant: &Variant,
    fmt_str: &str,
    fmt_args: &[TokenStream2],
) -> syn::Result<(TokenStream2, TokenStream2)> {
    let variant_name = &variant.ident;

    match &variant.fields {
        Fields::Unit => {
            // Unit variant with explicit args -- args are probably constants.
            let pattern = quote! { #enum_name::#variant_name };
            let display = quote! { ::core::write!(f, #fmt_str, #(#fmt_args),*) };
            Ok((pattern, display))
        }
        Fields::Unnamed(fields) => {
            let field_names: Vec<_> = (0..fields.unnamed.len())
                .map(|i| format_ident!("__field{}", i))
                .collect();
            let pattern = quote! { #enum_name::#variant_name(#(#field_names),*) };

            // Rewrite `.N` references in each arg to `__fieldN`
            let rewritten_args: Vec<TokenStream2> = fmt_args
                .iter()
                .map(|arg| rewrite_dot_field_refs(arg, &field_names))
                .collect();
            let display = quote! { ::core::write!(f, #fmt_str, #(#rewritten_args),*) };
            Ok((pattern, display))
        }
        Fields::Named(fields) => {
            let field_names: Vec<_> = fields
                .named
                .iter()
                .map(|f| f.ident.clone().expect("named field has ident"))
                .collect();

            // Bind all fields since we cannot easily determine which are used.
            let pattern = quote! { #enum_name::#variant_name { #(#field_names,)* .. } };

            // Rewrite `.field_name` references in each arg
            let rewritten_args: Vec<TokenStream2> = fmt_args
                .iter()
                .map(|arg| rewrite_dot_named_refs(arg, &field_names))
                .collect();
            let display = quote! { ::core::write!(f, #fmt_str, #(#rewritten_args),*) };
            Ok((pattern, display))
        }
    }
}

/// Rewrite `.N` shorthand to `__fieldN` in format arg token streams.
///
/// Handles patterns like `.0.display()` -> `__field0.display()`.
#[allow(clippy::collapsible_if)]
fn rewrite_dot_field_refs(tokens: &TokenStream2, field_names: &[syn::Ident]) -> TokenStream2 {
    let token_vec: Vec<proc_macro2::TokenTree> = tokens.clone().into_iter().collect();

    // Check if tokens start with `.` followed by a number literal
    if token_vec.len() >= 2 {
        if let proc_macro2::TokenTree::Punct(p) = &token_vec[0] {
            if p.as_char() == '.' {
                if let proc_macro2::TokenTree::Literal(lit) = &token_vec[1] {
                    let lit_str = lit.to_string();
                    if let Ok(idx) = lit_str.parse::<usize>() {
                        if idx < field_names.len() {
                            let field = &field_names[idx];
                            let rest: TokenStream2 = token_vec[2..].iter().cloned().collect();
                            return quote! { #field #rest };
                        }
                    }
                }
            }
        }
    }

    // No rewriting needed -- return as-is
    tokens.clone()
}

/// Rewrite `.field_name` shorthand to `field_name` in format arg token streams.
///
/// Handles patterns like `.field.method()` -> `field.method()`.
/// Named fields like `expected.0` are already valid since the field is bound.
#[allow(clippy::collapsible_if)]
fn rewrite_dot_named_refs(tokens: &TokenStream2, field_names: &[syn::Ident]) -> TokenStream2 {
    let token_vec: Vec<proc_macro2::TokenTree> = tokens.clone().into_iter().collect();

    // Check if tokens start with `.` followed by a known field name
    if token_vec.len() >= 2 {
        if let proc_macro2::TokenTree::Punct(p) = &token_vec[0] {
            if p.as_char() == '.' {
                if let proc_macro2::TokenTree::Ident(ident) = &token_vec[1] {
                    let is_field = field_names.iter().any(|f| f == ident);
                    if is_field {
                        let rest: TokenStream2 = token_vec[1..].iter().cloned().collect();
                        return rest;
                    }
                }
            }
        }
    }

    // No rewriting needed -- return as-is
    tokens.clone()
}

/// Build a `write!(f, ...)` call from a format string and field bindings.
///
/// For unnamed fields, we map `{0}` to `__field0`, `{1}` to `__field1`, etc.
/// For named fields, we pass them directly since `write!` supports named args.
/// Parse a `format!`-style template and return the set of argument NAMES it
/// references — both the `{name}` / `{name:spec}` placeholders AND any width /
/// precision parameters written as `name$` inside a spec (e.g. the `width` in
/// `{val:width$}`). Escaped braces `{{` / `}}` are correctly skipped, so
/// `{{field}}` references nothing.
///
/// This replaces the earlier substring heuristic (`contains("{name}")` /
/// `contains("{name:")`), which (a) MISSED `$`-parameter references — leaving
/// the field bound out with `..` and never passed, so `write!` failed to compile
/// ("there is no argument named ...") — and (b) FALSE-MATCHED escaped braces —
/// binding+passing a field the literal text never uses ("named argument never
/// used").
fn format_referenced_names(fmt: &str) -> std::collections::BTreeSet<String> {
    fn is_ident_start(c: char) -> bool {
        c.is_alphabetic() || c == '_'
    }
    fn is_ident_continue(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }
    let mut names = std::collections::BTreeSet::new();
    let cs: Vec<char> = fmt.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        match cs[i] {
            '{' => {
                if i + 1 < cs.len() && cs[i + 1] == '{' {
                    i += 2; // escaped "{{"
                    continue;
                }
                // Scan to the matching '}'.
                let start = i + 1;
                let mut j = start;
                while j < cs.len() && cs[j] != '}' {
                    j += 1;
                }
                if j >= cs.len() {
                    break; // unterminated placeholder; malformed template
                }
                // inner = "arg" | "arg:spec" | ":spec" | "".
                let inner: String = cs[start..j].iter().collect();
                let (arg, spec) = match inner.split_once(':') {
                    Some((a, s)) => (a.trim(), Some(s)),
                    None => (inner.trim(), None),
                };
                let mut ac = arg.chars();
                let arg_is_ident = match ac.next() {
                    Some(c) if is_ident_start(c) => ac.all(is_ident_continue),
                    _ => false,
                };
                if arg_is_ident {
                    names.insert(arg.to_string());
                }
                if let Some(spec) = spec {
                    // Collect width/precision parameter refs of the form `<ident>$`.
                    let sc: Vec<char> = spec.chars().collect();
                    let mut k = 0;
                    while k < sc.len() {
                        if is_ident_start(sc[k]) {
                            let s = k;
                            while k < sc.len() && is_ident_continue(sc[k]) {
                                k += 1;
                            }
                            if k < sc.len() && sc[k] == '$' {
                                names.insert(sc[s..k].iter().collect());
                                k += 1; // consume '$'
                            }
                        } else {
                            k += 1;
                        }
                    }
                }
                i = j + 1;
            }
            '}' => {
                // "}}" escaped close; a lone '}' is malformed — skip either way.
                i += if i + 1 < cs.len() && cs[i + 1] == '}' {
                    2
                } else {
                    1
                };
            }
            _ => i += 1,
        }
    }
    names
}

/// Emit the body for a template with NO field arguments. `write_str` is correct
/// only when the template has no escaped braces; a template containing `{{` or
/// `}}` must go through `write!` so the compiler unescapes them to single braces
/// (`write_str` would emit the doubled braces verbatim).
fn write_literal(fmt_str: &str) -> TokenStream2 {
    if fmt_str.contains("{{") || fmt_str.contains("}}") {
        quote! { ::core::write!(f, #fmt_str) }
    } else {
        quote! { f.write_str(#fmt_str) }
    }
}

fn build_format_call(
    fmt_str: &str,
    unnamed: &[syn::Ident],
    named: &[syn::Ident],
) -> syn::Result<TokenStream2> {
    if !unnamed.is_empty() {
        // For tuple variants: rewrite `{0}`, `{1}` etc. to named refs.
        // Also handle bare `{}` for single-field tuples.
        let mut rewritten = fmt_str.to_string();

        // Replace {N} with {__fieldN} for positional references
        for (i, ident) in unnamed.iter().enumerate() {
            let positional = format!("{{{i}}}");
            let named_ref = format!("{{{ident}}}");
            rewritten = rewritten.replace(&positional, &named_ref);
        }

        // Replace bare {} with {__field0} for single-field tuple variants
        if unnamed.len() == 1 && rewritten.contains("{}") {
            let first = &unnamed[0];
            rewritten = rewritten.replace("{}", &format!("{{{first}}}"));
        }

        // Replace {:?} with {__field0:?} for single-field tuple variants
        if unnamed.len() == 1 && rewritten.contains("{:?}") {
            let first = &unnamed[0];
            rewritten = rewritten.replace("{:?}", &format!("{{{first}:?}}"));
        }

        // Replace {0:?} with {__field0:?}
        for (i, ident) in unnamed.iter().enumerate() {
            let positional_debug = format!("{{{i}:?}}");
            let named_debug = format!("{{{ident}:?}}");
            rewritten = rewritten.replace(&positional_debug, &named_debug);
        }

        // Replace {0:02X} and similar format specifiers
        for (i, ident) in unnamed.iter().enumerate() {
            // Match {N:<spec>} patterns
            let prefix = format!("{{{i}:");
            while let Some(start) = rewritten.find(&prefix) {
                if let Some(end) = rewritten[start..].find('}') {
                    let spec = &rewritten[start + prefix.len()..start + end];
                    let replacement = format!("{{{ident}:{spec}}}");
                    rewritten = format!(
                        "{}{}{}",
                        &rewritten[..start],
                        replacement,
                        &rewritten[start + end + 1..]
                    );
                } else {
                    break;
                }
            }
        }

        // Only pass unnamed fields that are actually referenced in the rewritten format string.
        let refs = format_referenced_names(&rewritten);
        let referenced: Vec<_> = unnamed
            .iter()
            .filter(|ident| refs.contains(&ident.to_string()))
            .collect();

        Ok(quote! { ::core::write!(f, #rewritten, #(#referenced = #referenced),*) })
    } else if !named.is_empty() {
        // Only pass named fields that are actually referenced in the format string.
        // Fields with #[source] or #[from] may not appear in the format string.
        let refs = format_referenced_names(fmt_str);
        let referenced: Vec<_> = named
            .iter()
            .filter(|ident| refs.contains(&ident.to_string()))
            .collect();

        if referenced.is_empty() {
            Ok(write_literal(fmt_str))
        } else {
            Ok(quote! { ::core::write!(f, #fmt_str, #(#referenced = #referenced),*) })
        }
    } else {
        Ok(write_literal(fmt_str))
    }
}

/// Build the source() match arm for a variant.
fn build_source_arm(enum_name: &syn::Ident, variant: &Variant) -> syn::Result<TokenStream2> {
    let variant_name = &variant.ident;

    // Look for #[source] or #[from] fields
    match &variant.fields {
        Fields::Named(fields) => {
            for field in &fields.named {
                if has_attr(&field.attrs, "source") || has_attr(&field.attrs, "from") {
                    let field_name = field.ident.as_ref().expect("named field");
                    return Ok(quote! {
                        #enum_name::#variant_name { #field_name, .. } =>
                            ::core::option::Option::Some(#field_name),
                    });
                }
            }
        }
        Fields::Unnamed(fields) => {
            for (i, field) in fields.unnamed.iter().enumerate() {
                if has_attr(&field.attrs, "source") || has_attr(&field.attrs, "from") {
                    let idx = format_ident!("__field{}", i);
                    let bindings: Vec<_> = (0..fields.unnamed.len())
                        .map(|j| {
                            if j == i {
                                format_ident!("__field{}", j)
                            } else {
                                format_ident!("_")
                            }
                        })
                        .collect();
                    return Ok(quote! {
                        #enum_name::#variant_name(#(#bindings),*) =>
                            ::core::option::Option::Some(#idx),
                    });
                }
            }
        }
        Fields::Unit => {}
    }

    // No source field found -- this variant produces no source arm
    // (the catch-all _ => None handles it)
    Ok(TokenStream2::new())
}

/// Build the transparent pattern, returns (pattern, field_access).
fn build_transparent_pattern(
    enum_name: &syn::Ident,
    variant: &Variant,
) -> syn::Result<(TokenStream2, TokenStream2)> {
    let variant_name = &variant.ident;

    match &variant.fields {
        Fields::Unnamed(fields) if fields.unnamed.len() == 1 => {
            let pattern = quote! { #enum_name::#variant_name(__field0) };
            let access = quote! { __field0 };
            Ok((pattern, access))
        }
        Fields::Named(fields) if fields.named.len() == 1 => {
            let field_name = fields.named[0].ident.as_ref().expect("named field");
            let pattern = quote! { #enum_name::#variant_name { #field_name } };
            let access = quote! { #field_name };
            Ok((pattern, access))
        }
        _ => Err(syn::Error::new_spanned(
            variant_name,
            "#[error(transparent)] requires exactly one field",
        )),
    }
}

/// Generate `From<FieldType>` impls for fields marked with `#[from]`.
fn build_from_impls(
    enum_name: &syn::Ident,
    variant: &Variant,
    generics: &syn::Generics,
) -> syn::Result<Vec<TokenStream2>> {
    let variant_name = &variant.ident;
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let mut impls = Vec::new();

    match &variant.fields {
        Fields::Unnamed(fields) => {
            for (i, field) in fields.unnamed.iter().enumerate() {
                if has_attr(&field.attrs, "from") {
                    let ty = &field.ty;
                    let construction = if fields.unnamed.len() == 1 {
                        quote! { #enum_name::#variant_name(value) }
                    } else {
                        // Multi-field tuple with #[from] on one field: others get Default
                        let args: Vec<_> = (0..fields.unnamed.len())
                            .map(|j| {
                                if j == i {
                                    quote! { value }
                                } else {
                                    quote! { ::core::default::Default::default() }
                                }
                            })
                            .collect();
                        quote! { #enum_name::#variant_name(#(#args),*) }
                    };

                    impls.push(quote! {
                        impl #impl_generics ::core::convert::From<#ty> for #enum_name #ty_generics #where_clause {
                            fn from(value: #ty) -> Self {
                                #construction
                            }
                        }
                    });
                }
            }
        }
        Fields::Named(fields) => {
            for field in &fields.named {
                if has_attr(&field.attrs, "from") {
                    let ty = &field.ty;
                    let field_name = field.ident.as_ref().expect("named field");

                    // All other fields get Default
                    let other_fields: Vec<_> = fields
                        .named
                        .iter()
                        .filter(|f| f.ident.as_ref() != Some(field_name))
                        .map(|f| {
                            let name = f.ident.as_ref().expect("named field");
                            quote! { #name: ::core::default::Default::default() }
                        })
                        .collect();

                    impls.push(quote! {
                        impl #impl_generics ::core::convert::From<#ty> for #enum_name #ty_generics #where_clause {
                            fn from(value: #ty) -> Self {
                                #enum_name::#variant_name {
                                    #field_name: value,
                                    #(#other_fields),*
                                }
                            }
                        }
                    });
                }
            }
        }
        Fields::Unit => {}
    }

    Ok(impls)
}
