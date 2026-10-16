// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde_derive 1.0.228 src/de/enum_internally.rs and modified by
// the aterm project in 2026 (templates over std proc_macro; no variant-level
// `deserialize_with` or `skip`, no `expecting`).
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

//! Deserialization for internally tagged enums:
//!
//! ```ignore
//! #[serde(tag = "...")]
//! enum Enum {}
//! ```

use aterm_macro_support::q;
use proc_macro::{Literal, TokenStream};

use crate::ast::{Style, Variant};
use crate::attr;
use crate::de::enum_;
use crate::de::enum_untagged;
use crate::de::struct_;
use crate::de::{Parameters, StructForm, effective_style, expr_is_missing, field_i};
use crate::fragment::{Expr, Fragment, Match};

/// Generates `Deserialize::deserialize` body for an `enum Enum {...}` with `#[serde(tag)]` attribute
pub(super) fn deserialize(
    params: &Parameters,
    variants: &[Variant],
    cattrs: &attr::Container,
    tag: &str,
) -> Fragment {
    let (variants_stmt, variant_visitor) = enum_::prepare_enum_variant_enum(variants);

    // Match arms to extract a variant from a string
    let variant_arms: Vec<TokenStream> = variants
        .iter()
        .enumerate()
        .map(|(i, variant)| {
            let variant_name = field_i(i);

            let block = Match(deserialize_internally_tagged_variant(
                params, variant, cattrs,
            ));

            q!("__Field::#variant_name => #block", variant_name, block)
        })
        .collect();

    let expecting = format!("internally tagged enum {}", params.type_name());

    quote_block!(
        "#variant_visitor

        #variants_stmt

        let (__tag, __content) = _serde::Deserializer::deserialize_any(
            __deserializer,
            _serde::__private::de::TaggedContentVisitor::<__Field>::new(#tag, #expecting))?;
        let __deserializer = _serde::__private::de::ContentDeserializer::<__D::Error>::new(__content);

        match __tag {
            #variant_arms
        }",
        variant_visitor,
        variants_stmt,
        tag = Literal::string(tag),
        expecting = Literal::string(&expecting),
        variant_arms,
    )
}

// Generates significant part of the visit_seq and visit_map bodies of visitors
// for the variants of internally tagged enum.
fn deserialize_internally_tagged_variant(
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
            quote_block!(
                "_serde::Deserializer::deserialize_any(__deserializer, _serde::__private::de::InternallyTaggedUnitVisitor::new(#type_name, #variant_name))?;
                _serde::__private::Ok(#this_value::#variant_ident #default)",
                type_name,
                variant_name,
                this_value,
                variant_ident,
                default,
            )
        }
        Style::Newtype => {
            enum_untagged::deserialize_newtype_variant(variant_ident, params, &variant.fields[0])
        }
        Style::Struct => struct_::deserialize(
            params,
            &variant.fields,
            cattrs,
            &StructForm::InternallyTagged(variant_ident),
        ),
        // Refused when the attributes were read (`decide_tag`); a tuple
        // variant in an internally tagged enum never reaches here.
        Style::Tuple => quote_expr!(
            "_serde::__private::Err(_serde::de::Error::custom(\"tuple variant in an internally tagged enum\"))"
        ),
    }
}
