// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde_derive 1.0.228 src/de/tuple.rs and modified by the aterm
// project in 2026 (templates over std proc_macro; no getter, no `expecting`,
// no in-place deserialization).
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

use aterm_macro_support::{q, q_spanned};
use proc_macro::{Literal, TokenStream};

use crate::ast::Field;
use crate::attr;
use crate::de::{
    Parameters, TupleForm, deserialize_seq, span_of, visitor_expr, visitor_impl, visitor_struct,
};
use crate::fragment::{Fragment, Stmts};

/// Generates `Deserialize::deserialize` body for a `struct Tuple(...);` including `struct Newtype(T);`
pub(super) fn deserialize(
    params: &Parameters,
    fields: &[Field],
    cattrs: &attr::Container,
    form: &TupleForm,
) -> Fragment {
    // Tuples and tuple variants cannot have flatten fields: the checks refuse
    // `flatten` there before any code is generated.
    let field_count = fields
        .iter()
        .filter(|field| !field.attrs.skip_deserializing())
        .count();

    let this_value = &params.this_type;
    let delife = params.borrowed.de_lifetime();

    let construct = q!("#this_value", this_value);

    let type_path = match form {
        TupleForm::Tuple => construct,
        TupleForm::ExternallyTagged(variant_ident) | TupleForm::Untagged(variant_ident) => {
            q!("#construct::#variant_ident", construct, variant_ident)
        }
    };
    let expecting = match form {
        TupleForm::Tuple => format!("tuple struct {}", params.type_name()),
        TupleForm::ExternallyTagged(variant_ident) | TupleForm::Untagged(variant_ident) => {
            format!("tuple variant {}::{}", params.type_name(), variant_ident)
        }
    };

    let nfields = fields.len();

    let visit_newtype_struct = match form {
        TupleForm::Tuple if nfields == 1 => {
            Some(deserialize_newtype_struct(&type_path, params, &fields[0]))
        }
        _ => None,
    };

    let visit_seq = Stmts(deserialize_seq(
        &type_path, params, fields, false, cattrs, &expecting,
    ));

    let visitor_expr = visitor_expr(params);
    let field_count_lit = Literal::usize_suffixed(field_count);
    let dispatch = match form {
        TupleForm::Tuple if nfields == 1 => {
            let type_name = cattrs.name().deserialize_name();
            q!(
                "_serde::Deserializer::deserialize_newtype_struct(__deserializer, #type_name, #visitor_expr)",
                type_name,
                visitor_expr
            )
        }
        TupleForm::Tuple => {
            let type_name = cattrs.name().deserialize_name();
            q!(
                "_serde::Deserializer::deserialize_tuple_struct(__deserializer, #type_name, #field_count, #visitor_expr)",
                type_name,
                field_count = field_count_lit,
                visitor_expr
            )
        }
        TupleForm::ExternallyTagged(_) => q!(
            "_serde::de::VariantAccess::tuple_variant(__variant, #field_count, #visitor_expr)",
            field_count = field_count_lit,
            visitor_expr
        ),
        TupleForm::Untagged(_) => q!(
            "_serde::Deserializer::deserialize_tuple(__deserializer, #field_count, #visitor_expr)",
            field_count = field_count_lit,
            visitor_expr
        ),
    };

    let visitor_var = if field_count == 0 {
        q!("_")
    } else {
        q!("mut __seq")
    };

    let visitor_struct = visitor_struct(params);
    let visitor_impl = visitor_impl(
        params,
        &expecting,
        &q!(
            "#visit_newtype_struct

            #[inline]
            fn visit_seq<__A>(self, #visitor_var: __A) -> _serde::__private::Result<Self::Value, __A::Error>
            where
                __A: _serde::de::SeqAccess<#delife>,
            {
                #visit_seq
            }",
            visit_newtype_struct,
            visitor_var,
            delife,
            visit_seq,
        ),
    );

    quote_block!(
        "#visitor_struct

        #visitor_impl

        #dispatch",
        visitor_struct,
        visitor_impl,
        dispatch,
    )
}

fn deserialize_newtype_struct(
    type_path: &TokenStream,
    params: &Parameters,
    field: &Field,
) -> TokenStream {
    let delife = params.borrowed.de_lifetime();
    let field_ty = &field.ty;
    let deserializer_var = q!("__e");

    let value = match field.attrs.deserialize_with() {
        None => {
            let func = q_spanned!(field.span() => "<#field_ty as _serde::Deserialize>::deserialize", field_ty);
            q!("#func(#deserializer_var)?", func, deserializer_var)
        }
        Some(path) => {
            // If #path returns wrong type, error will be reported here (^^^^^).
            // We attach span of the path to the function so it will be reported
            // on the #[serde(deserialize_with = "...")]
            //                                   ^^^^^
            q_spanned!(span_of(path) => "#path(#deserializer_var)?", path, deserializer_var)
        }
    };

    let result = q!("#type_path(__field0)", type_path);

    q!(
        "#[inline]
        fn visit_newtype_struct<__E>(self, #deserializer_var: __E) -> _serde::__private::Result<Self::Value, __E::Error>
        where
            __E: _serde::Deserializer<#delife>,
        {
            let __field0: #field_ty = #value;
            _serde::__private::Ok(#result)
        }",
        deserializer_var,
        delife,
        field_ty,
        value,
        result,
    )
}
