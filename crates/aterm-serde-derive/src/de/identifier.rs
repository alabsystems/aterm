// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde_derive 1.0.228 src/de/identifier.rs and modified by the
// aterm project in 2026 (templates over std proc_macro; no
// `field_identifier`/`variant_identifier` enums, no `other`, no `expecting`).
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

//! Deserialization of struct field identifiers and enum variant identifiers by
//! way of a Rust enum.

use aterm_macro_support::q;
use proc_macro::{Ident, Literal, TokenStream};

use crate::de::FieldWithAliases;
use crate::fragment::{Fragment, Stmts};

pub(super) fn deserialize_generated(
    deserialized_fields: &[FieldWithAliases],
    has_flatten: bool,
    is_variant: bool,
    ignore_variant: Option<TokenStream>,
    fallthrough: Option<TokenStream>,
) -> Fragment {
    let this_value = q!("__Field");
    let field_idents: Vec<&Ident> = deserialized_fields
        .iter()
        .map(|field| &field.ident)
        .collect();
    let field_idents = aterm_macro_support::join(&field_idents, ',', true);

    let visitor_impl = Stmts(deserialize_identifier(
        &this_value,
        deserialized_fields,
        is_variant,
        fallthrough.as_ref(),
        !is_variant && has_flatten,
    ));

    let lifetime = if !is_variant && has_flatten {
        Some(q!("<'de>"))
    } else {
        None
    };

    quote_block!(
        "#[allow(non_camel_case_types)]
        #[doc(hidden)]
        enum __Field #lifetime {
            #field_idents
            #ignore_variant
        }

        #[doc(hidden)]
        struct __FieldVisitor;

        #[automatically_derived]
        impl<'de> _serde::de::Visitor<'de> for __FieldVisitor {
            type Value = __Field #lifetime;

            #visitor_impl
        }

        #[automatically_derived]
        impl<'de> _serde::Deserialize<'de> for __Field #lifetime {
            #[inline]
            fn deserialize<__D>(__deserializer: __D) -> _serde::__private::Result<Self, __D::Error>
            where
                __D: _serde::Deserializer<'de>,
            {
                _serde::Deserializer::deserialize_identifier(__deserializer, __FieldVisitor)
            }
        }",
        lifetime,
        field_idents,
        ignore_variant,
        visitor_impl,
    )
}

/// The `Content` variant and value type of each `visit_*` a flattening
/// struct's field identifier keeps for the flattened fields to read.
const OTHER_VISITS: &[(&str, &str, &str)] = &[
    ("visit_bool", "bool", "Bool"),
    ("visit_i8", "i8", "I8"),
    ("visit_i16", "i16", "I16"),
    ("visit_i32", "i32", "I32"),
    ("visit_i64", "i64", "I64"),
    ("visit_u8", "u8", "U8"),
    ("visit_u16", "u16", "U16"),
    ("visit_u32", "u32", "U32"),
    ("visit_u64", "u64", "U64"),
    ("visit_f32", "f32", "F32"),
    ("visit_f64", "f64", "F64"),
    ("visit_char", "char", "Char"),
];

fn deserialize_identifier(
    this_value: &TokenStream,
    deserialized_fields: &[FieldWithAliases],
    is_variant: bool,
    fallthrough: Option<&TokenStream>,
    collect_other_fields: bool,
) -> Fragment {
    let str_mapping: Vec<TokenStream> = deserialized_fields
        .iter()
        .flat_map(|field| {
            let ident = &field.ident;
            field.aliases.iter().map(move |alias| {
                q!(
                    "#alias => _serde::__private::Ok(#this_value::#ident),",
                    alias,
                    this_value,
                    ident
                )
            })
        })
        .collect();
    let bytes_mapping: Vec<TokenStream> = deserialized_fields
        .iter()
        .flat_map(|field| {
            let ident = &field.ident;
            field.aliases.iter().map(move |alias| {
                let alias = Literal::byte_string(alias.value.as_bytes());
                q!(
                    "#alias => _serde::__private::Ok(#this_value::#ident),",
                    alias,
                    this_value,
                    ident
                )
            })
        })
        .collect();

    let expecting = Literal::string(if is_variant {
        "variant identifier"
    } else {
        "field identifier"
    });

    let bytes_to_str = if fallthrough.is_some() || collect_other_fields {
        None
    } else {
        Some(q!(
            "let __value = &_serde::__private::from_utf8_lossy(__value);"
        ))
    };

    let (
        value_as_str_content,
        value_as_borrowed_str_content,
        value_as_bytes_content,
        value_as_borrowed_bytes_content,
    ) = if collect_other_fields {
        (
            Some(q!(
                "let __value = _serde::__private::de::Content::String(_serde::__private::ToString::to_string(__value));"
            )),
            Some(q!(
                "let __value = _serde::__private::de::Content::Str(__value);"
            )),
            Some(q!(
                "let __value = _serde::__private::de::Content::ByteBuf(__value.to_vec());"
            )),
            Some(q!(
                "let __value = _serde::__private::de::Content::Bytes(__value);"
            )),
        )
    } else {
        (None, None, None, None)
    };

    let fallthrough_arm = match fallthrough {
        Some(fallthrough) => fallthrough.clone(),
        None if is_variant => {
            q!("_serde::__private::Err(_serde::de::Error::unknown_variant(__value, VARIANTS))")
        }
        None => q!("_serde::__private::Err(_serde::de::Error::unknown_field(__value, FIELDS))"),
    };

    let visit_other = if collect_other_fields {
        let mut visits: Vec<TokenStream> = OTHER_VISITS
            .iter()
            .map(|(method, ty, content)| {
                let method = Ident::new(method, proc_macro::Span::call_site());
                let ty = Ident::new(ty, proc_macro::Span::call_site());
                let content = Ident::new(content, proc_macro::Span::call_site());
                q!(
                    "fn #method<__E>(self, __value: #ty) -> _serde::__private::Result<Self::Value, __E>
                    where
                        __E: _serde::de::Error,
                    {
                        _serde::__private::Ok(__Field::__other(_serde::__private::de::Content::#content(__value)))
                    }",
                    method,
                    ty,
                    content
                )
            })
            .collect();
        visits.push(q!(
            "fn visit_unit<__E>(self) -> _serde::__private::Result<Self::Value, __E>
            where
                __E: _serde::de::Error,
            {
                _serde::__private::Ok(__Field::__other(_serde::__private::de::Content::Unit))
            }"
        ));
        visits.into_iter().collect()
    } else {
        let u64_mapping: Vec<TokenStream> = deserialized_fields
            .iter()
            .enumerate()
            .map(|(i, field)| {
                let i = Literal::u64_suffixed(i as u64);
                let ident = &field.ident;
                q!(
                    "#i => _serde::__private::Ok(#this_value::#ident),",
                    i,
                    this_value,
                    ident
                )
            })
            .collect();

        let u64_fallthrough_arm = match fallthrough {
            Some(fallthrough) => fallthrough.clone(),
            None => {
                let index_expecting = if is_variant { "variant" } else { "field" };
                let fallthrough_msg = Literal::string(&format!(
                    "{} index 0 <= i < {}",
                    index_expecting,
                    deserialized_fields.len(),
                ));
                q!(
                    "_serde::__private::Err(_serde::de::Error::invalid_value(
                        _serde::de::Unexpected::Unsigned(__value),
                        &#fallthrough_msg,
                    ))",
                    fallthrough_msg
                )
            }
        };

        q!(
            "fn visit_u64<__E>(self, __value: u64) -> _serde::__private::Result<Self::Value, __E>
            where
                __E: _serde::de::Error,
            {
                match __value {
                    #u64_mapping
                    _ => #u64_fallthrough_arm,
                }
            }",
            u64_mapping,
            u64_fallthrough_arm
        )
    };

    let visit_borrowed = if collect_other_fields {
        Some(q!(
            "fn visit_borrowed_str<__E>(self, __value: &'de str) -> _serde::__private::Result<Self::Value, __E>
            where
                __E: _serde::de::Error,
            {
                match __value {
                    #str_mapping
                    _ => {
                        #value_as_borrowed_str_content
                        #fallthrough_arm
                    }
                }
            }

            fn visit_borrowed_bytes<__E>(self, __value: &'de [u8]) -> _serde::__private::Result<Self::Value, __E>
            where
                __E: _serde::de::Error,
            {
                match __value {
                    #bytes_mapping
                    _ => {
                        #bytes_to_str
                        #value_as_borrowed_bytes_content
                        #fallthrough_arm
                    }
                }
            }",
            str_mapping,
            value_as_borrowed_str_content,
            fallthrough_arm,
            bytes_mapping,
            bytes_to_str,
            value_as_borrowed_bytes_content,
        ))
    } else {
        None
    };

    quote_block!(
        "fn expecting(&self, __formatter: &mut _serde::__private::Formatter) -> _serde::__private::fmt::Result {
            _serde::__private::Formatter::write_str(__formatter, #expecting)
        }

        #visit_other

        fn visit_str<__E>(self, __value: &str) -> _serde::__private::Result<Self::Value, __E>
        where
            __E: _serde::de::Error,
        {
            match __value {
                #str_mapping
                _ => {
                    #value_as_str_content
                    #fallthrough_arm
                }
            }
        }

        fn visit_bytes<__E>(self, __value: &[u8]) -> _serde::__private::Result<Self::Value, __E>
        where
            __E: _serde::de::Error,
        {
            match __value {
                #bytes_mapping
                _ => {
                    #bytes_to_str
                    #value_as_bytes_content
                    #fallthrough_arm
                }
            }
        }

        #visit_borrowed",
        expecting,
        visit_other,
        str_mapping,
        value_as_str_content,
        fallthrough_arm,
        bytes_mapping,
        bytes_to_str,
        value_as_bytes_content,
        visit_borrowed,
    )
}
