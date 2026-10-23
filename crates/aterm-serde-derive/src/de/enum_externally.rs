// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde_derive 1.0.228 src/de/enum_externally.rs and modified by
// the aterm project in 2026 (templates over std proc_macro; no variant-level
// `deserialize_with` or `skip`, no `expecting`).
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

//! Deserialization for externally tagged enums:
//!
//! ```
//! # extern crate aterm_serde as serde;
//! #[derive(aterm_serde_derive::Deserialize)]
//! enum Event {
//!     Ready,
//!     Data { value: u32 },
//! }
//! # fn assert_deserializable<T: for<'de> serde::Deserialize<'de>>() {}
//! # assert_deserializable::<Event>();
//! ```
//! # extern crate aterm_serde as serde;
//! #[derive(aterm_serde_derive::Deserialize)]
//! enum Event {
//!     Ready,
//!     Data { value: u32 },
//! }
//! # fn assert_deserializable<T: for<'de> serde::Deserialize<'de>>() {}
//! # assert_deserializable::<Event>();

use aterm_macro_support::{q, q_spanned};
use proc_macro::{Ident, TokenStream};

use crate::ast::{Field, Style, Variant};
use crate::attr;
use crate::de::enum_;
use crate::de::struct_;
use crate::de::tuple;
use crate::de::{
    Parameters, StructForm, TupleForm, expr_is_missing, field_i, visitor_expr, visitor_impl,
    visitor_struct, wrap_deserialize_field_with,
};
use crate::fragment::{Expr, Fragment, Match};

/// Generates `Deserialize::deserialize` body for an `enum Enum {...}` without additional attributes
pub(super) fn deserialize(
    params: &Parameters,
    variants: &[Variant],
    cattrs: &attr::Container,
) -> Fragment {
    let delife = params.borrowed.de_lifetime();

    let type_name = cattrs.name().deserialize_name();
    let expecting = format!("enum {}", params.type_name());

    let (variants_stmt, variant_visitor) = enum_::prepare_enum_variant_enum(variants);

    // Match arms to extract a variant from a string
    let variant_arms: Vec<TokenStream> = variants
        .iter()
        .enumerate()
        .map(|(i, variant)| {
            let variant_name = field_i(i);

            let block = Match(deserialize_externally_tagged_variant(
                params, variant, cattrs,
            ));

            q!(
                "(__Field::#variant_name, __variant) => #block",
                variant_name,
                block
            )
        })
        .collect();

    let match_variant = if variants.is_empty() {
        // This is an empty enum like `enum Impossible {}`.
        q!("_serde::__private::Result::map(
                _serde::de::EnumAccess::variant::<__Field>(__data),
                |(__impossible, _)| match __impossible {})")
    } else {
        q!(
            "match _serde::de::EnumAccess::variant(__data)? {
                #variant_arms
            }",
            variant_arms
        )
    };

    let visitor_struct = visitor_struct(params);
    let visitor_impl = visitor_impl(
        params,
        &expecting,
        &q!(
            "fn visit_enum<__A>(self, __data: __A) -> _serde::__private::Result<Self::Value, __A::Error>
            where
                __A: _serde::de::EnumAccess<#delife>,
            {
                #match_variant
            }",
            delife,
            match_variant
        ),
    );
    let visitor_expr = visitor_expr(params);

    quote_block!(
        "#variant_visitor

        #visitor_struct

        #visitor_impl

        #variants_stmt

        _serde::Deserializer::deserialize_enum(
            __deserializer,
            #type_name,
            VARIANTS,
            #visitor_expr,
        )",
        variant_visitor,
        visitor_struct,
        visitor_impl,
        variants_stmt,
        type_name,
        visitor_expr,
    )
}

fn deserialize_externally_tagged_variant(
    params: &Parameters,
    variant: &Variant,
    cattrs: &attr::Container,
) -> Fragment {
    let variant_ident = &variant.ident;

    match variant.style {
        Style::Unit => {
            let this_value = &params.this_type;
            quote_block!(
                "_serde::de::VariantAccess::unit_variant(__variant)?;
                _serde::__private::Ok(#this_value::#variant_ident)",
                this_value,
                variant_ident
            )
        }
        Style::Newtype => deserialize_externally_tagged_newtype_variant(
            variant_ident,
            params,
            &variant.fields[0],
            cattrs,
        ),
        Style::Tuple => tuple::deserialize(
            params,
            &variant.fields,
            cattrs,
            &TupleForm::ExternallyTagged(variant_ident),
        ),
        Style::Struct => struct_::deserialize(
            params,
            &variant.fields,
            cattrs,
            &StructForm::ExternallyTagged(variant_ident),
        ),
    }
}

fn deserialize_externally_tagged_newtype_variant(
    variant_ident: &Ident,
    params: &Parameters,
    field: &Field,
    cattrs: &attr::Container,
) -> Fragment {
    let this_value = &params.this_type;

    if field.attrs.skip_deserializing() {
        let default = Expr(expr_is_missing(field, cattrs));
        return quote_block!(
            "_serde::de::VariantAccess::unit_variant(__variant)?;
            _serde::__private::Ok(#this_value::#variant_ident(#default))",
            this_value,
            variant_ident,
            default
        );
    }

    match field.attrs.deserialize_with() {
        None => {
            let field_ty = &field.ty;
            let func = q_spanned!(field.span() => "_serde::de::VariantAccess::newtype_variant::<#field_ty>", field_ty);
            quote_expr!(
                "_serde::__private::Result::map(#func(__variant), #this_value::#variant_ident)",
                func,
                this_value,
                variant_ident
            )
        }
        Some(path) => {
            let (wrapper, wrapper_ty) = wrap_deserialize_field_with(params, &field.ty, path);
            quote_block!(
                "#wrapper
                _serde::__private::Result::map(
                    _serde::de::VariantAccess::newtype_variant::<#wrapper_ty>(__variant),
                    |__wrapper| #this_value::#variant_ident(__wrapper.value))",
                wrapper,
                wrapper_ty,
                this_value,
                variant_ident
            )
        }
    }
}
