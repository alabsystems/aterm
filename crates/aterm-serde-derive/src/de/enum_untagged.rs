// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde_derive 1.0.228 src/de/enum_untagged.rs and modified by
// the aterm project in 2026 (templates over std proc_macro; no variant-level
// `deserialize_with` or `skip`, no `expecting`, no partially untagged enums).
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

//! Deserialization for untagged enums:
//!
//! ```
//! # extern crate aterm_serde as serde;
//! #[derive(aterm_serde_derive::Deserialize)]
//! #[serde(untagged)]
//! enum Value {
//!     Number(u32),
//!     Text(String),
//! }
//! # fn assert_deserializable<T: for<'de> serde::Deserialize<'de>>() {}
//! # assert_deserializable::<Value>();
//! ```

use aterm_macro_support::{q, q_spanned};
use proc_macro::{Ident, Literal, TokenStream};

use crate::ast::{Field, Style, Variant};
use crate::attr;
use crate::de::struct_;
use crate::de::tuple;
use crate::de::{Parameters, StructForm, TupleForm, effective_style, expr_is_missing};
use crate::fragment::{Expr, Fragment};

/// Generates `Deserialize::deserialize` body for an `enum Enum {...}` with `#[serde(untagged)]` attribute
pub(super) fn deserialize(
    params: &Parameters,
    variants: &[Variant],
    cattrs: &attr::Container,
) -> Fragment {
    let attempts: Vec<TokenStream> = variants
        .iter()
        .map(|variant| {
            let attempt = Expr(deserialize_variant(params, variant, cattrs));
            q!(
                "if let _serde::__private::Ok(__ok) = #attempt {
                    return _serde::__private::Ok(__ok);
                }",
                attempt
            )
        })
        .collect();
    // TODO this message could be better by saving the errors from the failed
    // attempts. The heuristic used by TOML was to count the number of fields
    // processed before an error, and use the error that happened after the
    // largest number of fields. I'm not sure I like that. Maybe it would be
    // better to save all the errors and combine them into one message that
    // explains why none of the variants matched.
    let fallthrough_msg = format!(
        "data did not match any variant of untagged enum {}",
        params.type_name()
    );

    quote_block!(
        "let __content = _serde::de::DeserializeSeed::deserialize(_serde::__private::de::ContentVisitor::new(), __deserializer)?;
        let __deserializer = _serde::__private::de::ContentRefDeserializer::<__D::Error>::new(&__content);

        #attempts

        _serde::__private::Err(_serde::de::Error::custom(#fallthrough_msg))",
        attempts,
        fallthrough_msg = Literal::string(&fallthrough_msg),
    )
}

fn deserialize_variant(
    params: &Parameters,
    variant: &Variant,
    cattrs: &attr::Container,
) -> Fragment {
    let variant_ident = &variant.ident;

    match effective_style(variant) {
        Style::Unit => {
            let this_value = &params.this_type;
            let type_name = Literal::string(&params.type_name());
            let variant_name = Literal::string(&variant.ident.to_string());
            let default = variant.fields.first().map(|field| {
                let default = Expr(expr_is_missing(field, cattrs));
                q!("(#default)", default)
            });
            quote_expr!(
                "match _serde::Deserializer::deserialize_any(
                    __deserializer,
                    _serde::__private::de::UntaggedUnitVisitor::new(#type_name, #variant_name)
                ) {
                    _serde::__private::Ok(()) => _serde::__private::Ok(#this_value::#variant_ident #default),
                    _serde::__private::Err(__err) => _serde::__private::Err(__err),
                }",
                type_name,
                variant_name,
                this_value,
                variant_ident,
                default,
            )
        }
        Style::Newtype => deserialize_newtype_variant(variant_ident, params, &variant.fields[0]),
        Style::Tuple => tuple::deserialize(
            params,
            &variant.fields,
            cattrs,
            &TupleForm::Untagged(variant_ident),
        ),
        Style::Struct => struct_::deserialize(
            params,
            &variant.fields,
            cattrs,
            &StructForm::Untagged(variant_ident),
        ),
    }
}

// Also used by internally tagged enums
pub(super) fn deserialize_newtype_variant(
    variant_ident: &Ident,
    params: &Parameters,
    field: &Field,
) -> Fragment {
    let this_value = &params.this_type;
    let field_ty = &field.ty;
    match field.attrs.deserialize_with() {
        None => {
            let func = q_spanned!(field.span() => "<#field_ty as _serde::Deserialize>::deserialize", field_ty);
            quote_expr!(
                "_serde::__private::Result::map(#func(__deserializer), #this_value::#variant_ident)",
                func,
                this_value,
                variant_ident
            )
        }
        Some(path) => quote_block!(
            "let __value: _serde::__private::Result<#field_ty, _> = #path(__deserializer);
            _serde::__private::Result::map(__value, #this_value::#variant_ident)",
            field_ty,
            path,
            this_value,
            variant_ident
        ),
    }
}
