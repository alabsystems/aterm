// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde_derive 1.0.228 src/de/struct_.rs and modified by the
// aterm project in 2026 (templates over std proc_macro; no getter, no
// `expecting`, no in-place deserialization).
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

use aterm_macro_support::{q, q_spanned};
use proc_macro::{Literal, TokenStream};

use crate::ast::Field;
use crate::attr;
use crate::de::identifier;
use crate::de::{
    FieldWithAliases, Parameters, StructForm, deserialize_seq, expr_is_missing, field_i,
    has_flatten, let_default, visitor_expr, visitor_impl, visitor_struct,
    wrap_deserialize_field_with,
};
use crate::fragment::{Fragment, Match, Stmts};

/// Generates `Deserialize::deserialize` body for a `struct Struct {...}`
pub(super) fn deserialize(
    params: &Parameters,
    fields: &[Field],
    cattrs: &attr::Container,
    form: &StructForm,
) -> Fragment {
    let this_type = &params.this_type;
    let this_value = &params.this_type;
    let de_impl_generics = params.de_impl_generics();
    let de_ty_generics = params.de_ty_generics();
    let ty_generics = params.generics.ty_generics();
    let where_clause = params.generics.where_clause();
    let delife = params.borrowed.de_lifetime();

    let construct = q!("#this_value", this_value);

    let type_path = match form {
        StructForm::Struct => construct,
        StructForm::ExternallyTagged(variant_ident)
        | StructForm::InternallyTagged(variant_ident)
        | StructForm::Untagged(variant_ident) => {
            q!("#construct::#variant_ident", construct, variant_ident)
        }
    };
    let expecting = match form {
        StructForm::Struct => format!("struct {}", params.type_name()),
        StructForm::ExternallyTagged(variant_ident)
        | StructForm::InternallyTagged(variant_ident)
        | StructForm::Untagged(variant_ident) => {
            format!("struct variant {}::{}", params.type_name(), variant_ident)
        }
    };

    let deserialized_fields: Vec<_> = fields
        .iter()
        .enumerate()
        // Skip fields that shouldn't be deserialized or that were flattened,
        // so they don't appear in the storage in their literal form
        .filter(|&(_, field)| !field.attrs.skip_deserializing() && !field.attrs.flatten())
        .map(|(i, field)| FieldWithAliases {
            ident: field_i(i),
            aliases: field.attrs.aliases(),
        })
        .collect();

    let has_flatten = has_flatten(fields);
    let field_visitor = deserialize_field_identifier(&deserialized_fields, cattrs, has_flatten);

    // untagged struct variants do not get a visit_seq method. The same applies to
    // structs that only have a map representation.
    let visit_seq = match form {
        StructForm::Untagged(_) => None,
        _ if has_flatten => None,
        _ => {
            let mut_seq = if deserialized_fields.is_empty() {
                q!("_")
            } else {
                q!("mut __seq")
            };

            let visit_seq = Stmts(deserialize_seq(
                &type_path, params, fields, true, cattrs, &expecting,
            ));

            Some(q!(
                "#[inline]
                fn visit_seq<__A>(self, #mut_seq: __A) -> _serde::__private::Result<Self::Value, __A::Error>
                where
                    __A: _serde::de::SeqAccess<#delife>,
                {
                    #visit_seq
                }",
                mut_seq,
                delife,
                visit_seq,
            ))
        }
    };
    let visit_map = Stmts(deserialize_map(
        &type_path,
        params,
        fields,
        cattrs,
        has_flatten,
    ));

    let visitor_seed = match form {
        StructForm::ExternallyTagged(..) if has_flatten => Some(q!(
            "#[automatically_derived]
            impl #de_impl_generics _serde::de::DeserializeSeed<#delife> for __Visitor #de_ty_generics #where_clause {
                type Value = #this_type #ty_generics;

                fn deserialize<__D>(self, __deserializer: __D) -> _serde::__private::Result<Self::Value, __D::Error>
                where
                    __D: _serde::Deserializer<#delife>,
                {
                    _serde::Deserializer::deserialize_map(__deserializer, self)
                }
            }",
            de_impl_generics,
            delife,
            de_ty_generics,
            where_clause,
            this_type,
            ty_generics,
        )),
        _ => None,
    };

    let fields_stmt = if has_flatten {
        None
    } else {
        let field_names: Vec<&attr::Name> = deserialized_fields
            .iter()
            .flat_map(|field| field.aliases)
            .collect();
        let field_names = aterm_macro_support::join(&field_names, ',', false);

        Some(q!(
            "#[doc(hidden)]
            const FIELDS: &'static [&'static str] = &[ #field_names ];",
            field_names
        ))
    };

    let visitor_expr = visitor_expr(params);
    let dispatch = match form {
        StructForm::Struct if has_flatten => q!(
            "_serde::Deserializer::deserialize_map(__deserializer, #visitor_expr)",
            visitor_expr
        ),
        StructForm::Struct => {
            let type_name = cattrs.name().deserialize_name();
            q!(
                "_serde::Deserializer::deserialize_struct(__deserializer, #type_name, FIELDS, #visitor_expr)",
                type_name,
                visitor_expr
            )
        }
        StructForm::ExternallyTagged(_) if has_flatten => q!(
            "_serde::de::VariantAccess::newtype_variant_seed(__variant, #visitor_expr)",
            visitor_expr
        ),
        StructForm::ExternallyTagged(_) => q!(
            "_serde::de::VariantAccess::struct_variant(__variant, FIELDS, #visitor_expr)",
            visitor_expr
        ),
        StructForm::InternallyTagged(_) | StructForm::Untagged(_) => q!(
            "_serde::Deserializer::deserialize_any(__deserializer, #visitor_expr)",
            visitor_expr
        ),
    };

    let visitor_struct = visitor_struct(params);
    let visitor_impl = visitor_impl(
        params,
        &expecting,
        &q!(
            "#visit_seq

            #[inline]
            fn visit_map<__A>(self, mut __map: __A) -> _serde::__private::Result<Self::Value, __A::Error>
            where
                __A: _serde::de::MapAccess<#delife>,
            {
                #visit_map
            }",
            visit_seq,
            delife,
            visit_map,
        ),
    );

    quote_block!(
        "#field_visitor

        #visitor_struct

        #visitor_impl

        #visitor_seed

        #fields_stmt

        #dispatch",
        field_visitor,
        visitor_struct,
        visitor_impl,
        visitor_seed,
        fields_stmt,
        dispatch,
    )
}

fn deserialize_map(
    struct_path: &TokenStream,
    params: &Parameters,
    fields: &[Field],
    cattrs: &attr::Container,
    has_flatten: bool,
) -> Fragment {
    // Create the field names for the fields.
    let fields_names: Vec<_> = fields
        .iter()
        .enumerate()
        .map(|(i, field)| (field, field_i(i)))
        .collect();

    // Declare each field that will be deserialized.
    let let_values: Vec<TokenStream> = fields_names
        .iter()
        .filter(|&&(field, _)| !field.attrs.skip_deserializing() && !field.attrs.flatten())
        .map(|(field, name)| {
            let field_ty = &field.ty;
            q!(
                "let mut #name: _serde::__private::Option<#field_ty> = _serde::__private::None;",
                name,
                field_ty
            )
        })
        .collect();

    // Collect contents for flatten fields into a buffer
    let let_collect = if has_flatten {
        Some(q!(
            "let mut __collect = _serde::__private::Vec::<_serde::__private::Option<(
                _serde::__private::de::Content,
                _serde::__private::de::Content
            )>>::new();"
        ))
    } else {
        None
    };

    // Match arms to extract a value for a field.
    let value_arms: Vec<TokenStream> = fields_names
        .iter()
        .filter(|&&(field, _)| !field.attrs.skip_deserializing() && !field.attrs.flatten())
        .map(|(field, name)| {
            let deser_name = field.attrs.name().deserialize_name();

            let visit = match field.attrs.deserialize_with() {
                None => {
                    let field_ty = &field.ty;
                    let func = q_spanned!(field.span() => "_serde::de::MapAccess::next_value::<#field_ty>", field_ty);
                    q!("#func(&mut __map)?", func)
                }
                Some(path) => {
                    let (wrapper, wrapper_ty) = wrap_deserialize_field_with(params, &field.ty, path);
                    q!(
                        "{
                            #wrapper
                            match _serde::de::MapAccess::next_value::<#wrapper_ty>(&mut __map) {
                                _serde::__private::Ok(__wrapper) => __wrapper.value,
                                _serde::__private::Err(__err) => {
                                    return _serde::__private::Err(__err);
                                }
                            }
                        }",
                        wrapper,
                        wrapper_ty
                    )
                }
            };
            q!(
                "__Field::#name => {
                    if _serde::__private::Option::is_some(&#name) {
                        return _serde::__private::Err(<__A::Error as _serde::de::Error>::duplicate_field(#deser_name));
                    }
                    #name = _serde::__private::Some(#visit);
                }",
                name,
                deser_name,
                visit
            )
        })
        .collect();

    // Visit ignored values to consume them
    let ignored_arm = if has_flatten {
        Some(q!(
            "__Field::__other(__name) => {
                __collect.push(_serde::__private::Some((
                    __name,
                    _serde::de::MapAccess::next_value_seed(&mut __map, _serde::__private::de::ContentVisitor::new())?)));
            }"
        ))
    } else if cattrs.deny_unknown_fields() {
        None
    } else {
        Some(q!(
            "_ => { let _ = _serde::de::MapAccess::next_value::<_serde::de::IgnoredAny>(&mut __map)?; }"
        ))
    };

    let all_skipped = fields.iter().all(|field| field.attrs.skip_deserializing());
    let match_keys = if cattrs.deny_unknown_fields() && all_skipped {
        q!("_serde::__private::Option::map(
                _serde::de::MapAccess::next_key::<__Field>(&mut __map)?,
                |__impossible| match __impossible {});")
    } else {
        q!(
            "while let _serde::__private::Some(__key) = _serde::de::MapAccess::next_key::<__Field>(&mut __map)? {
                match __key {
                    #value_arms
                    #ignored_arm
                }
            }",
            value_arms,
            ignored_arm
        )
    };

    let extract_values: Vec<TokenStream> = fields_names
        .iter()
        .filter(|&&(field, _)| !field.attrs.skip_deserializing() && !field.attrs.flatten())
        .map(|(field, name)| {
            let missing_expr = Match(expr_is_missing(field, cattrs));

            q!(
                "let #name = match #name {
                    _serde::__private::Some(#name) => #name,
                    _serde::__private::None => #missing_expr
                };",
                name,
                missing_expr
            )
        })
        .collect();

    let extract_collected: Vec<TokenStream> = fields_names
        .iter()
        .filter(|&&(field, _)| field.attrs.flatten() && !field.attrs.skip_deserializing())
        .map(|(field, name)| {
            let field_ty = &field.ty;
            let func = match field.attrs.deserialize_with() {
                None => q_spanned!(field.span() => "_serde::de::Deserialize::deserialize"),
                Some(path) => path.clone(),
            };
            q!(
                "let #name: #field_ty = #func(
                    _serde::__private::de::FlatMapDeserializer(
                        &mut __collect,
                        _serde::__private::PhantomData))?;",
                name,
                field_ty,
                func
            )
        })
        .collect();

    let collected_deny_unknown_fields = if has_flatten && cattrs.deny_unknown_fields() {
        let unknown = Literal::string("unknown field `{}`");
        let unexpected = Literal::string("unexpected map key");
        Some(q!(
            "if let _serde::__private::Some(_serde::__private::Some((__key, _))) =
                __collect.into_iter().filter(_serde::__private::Option::is_some).next()
            {
                if let _serde::__private::Some(__key) = _serde::__private::de::content_as_str(&__key) {
                    return _serde::__private::Err(
                        _serde::de::Error::custom(format_args!(#unknown, &__key)));
                } else {
                    return _serde::__private::Err(
                        _serde::de::Error::custom(format_args!(#unexpected)));
                }
            }",
            unknown,
            unexpected
        ))
    } else {
        None
    };

    let result: Vec<TokenStream> = fields_names
        .iter()
        .map(|(field, name)| {
            let member = &field.member;
            if field.attrs.skip_deserializing() {
                let value = crate::fragment::Expr(expr_is_missing(field, cattrs));
                q!("#member: #value", member, value)
            } else {
                q!("#member: #name", member, name)
            }
        })
        .collect();
    let result = aterm_macro_support::join(&result, ',', false);

    let let_default = let_default(cattrs);

    let result = q!("#struct_path { #result }", struct_path, result);

    quote_block!(
        "#let_values

        #let_collect

        #match_keys

        #let_default

        #extract_values

        #extract_collected

        #collected_deny_unknown_fields

        _serde::__private::Ok(#result)",
        let_values,
        let_collect,
        match_keys,
        let_default,
        extract_values,
        extract_collected,
        collected_deny_unknown_fields,
        result,
    )
}

/// Generates enum and its `Deserialize` implementation that represents each
/// non-skipped field of the struct
fn deserialize_field_identifier(
    deserialized_fields: &[FieldWithAliases],
    cattrs: &attr::Container,
    has_flatten: bool,
) -> Stmts {
    let (ignore_variant, fallthrough) = if has_flatten {
        let ignore_variant = q!("__other(_serde::__private::de::Content<'de>),");
        let fallthrough = q!("_serde::__private::Ok(__Field::__other(__value))");
        (Some(ignore_variant), Some(fallthrough))
    } else if cattrs.deny_unknown_fields() {
        (None, None)
    } else {
        let ignore_variant = q!("__ignore,");
        let fallthrough = q!("_serde::__private::Ok(__Field::__ignore)");
        (Some(ignore_variant), Some(fallthrough))
    };

    Stmts(identifier::deserialize_generated(
        deserialized_fields,
        has_flatten,
        false,
        ignore_variant,
        fallthrough,
    ))
}
