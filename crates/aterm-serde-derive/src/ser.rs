// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde_derive 1.0.228 src/ser.rs and modified by the aterm
// project in 2026: templates over std proc_macro instead of quote, and only
// the paths aterm's attribute set reaches (no remote, getter, into,
// serialize_with, adjacent tagging or variant-level skip).
// See ../LICENSE-MIT and ../LICENSE-APACHE for the upstream licenses.

use aterm_macro_support::{DeriveInput, Errors, Generics, ToTokens, q, q_spanned};
use proc_macro::{Ident, Literal, Span, TokenStream};

use crate::ast::{Container, Data, Derive, Field, Member, Style, Variant};
use crate::attr::{self, Name};
use crate::bound;
use crate::fragment::{Fragment, Match, Stmts};

pub fn expand_derive_serialize(input: &DeriveInput) -> Result<TokenStream, TokenStream> {
    let cx = Errors::new();
    let Some(cont) = Container::from_ast(&cx, input, Derive::Serialize) else {
        return Err(cx.finish().err().unwrap_or_default());
    };
    cx.finish()?;

    let ident = &cont.ident;
    let params = Parameters::new(&cont);
    let impl_generics = params.generics.impl_generics();
    let ty_generics = params.generics.ty_generics();
    let where_clause = params.generics.where_clause();
    let body = Stmts(serialize_body(&cont, &params));
    let allow_deprecated = crate::allow_deprecated(input);

    let impl_block = q!(
        "#[automatically_derived]
        #allow_deprecated
        impl #impl_generics _serde::Serialize for #ident #ty_generics #where_clause {
            fn serialize<__S>(&self, __serializer: __S) -> _serde::__private::Result<__S::Ok, __S::Error>
            where
                __S: _serde::Serializer,
            {
                #body
            }
        }",
        allow_deprecated,
        impl_generics,
        ident,
        ty_generics,
        where_clause,
        body,
    );

    Ok(crate::wrap_in_const(&impl_block))
}

struct Parameters {
    /// Variable holding the value being serialized: `self`.
    self_var: Ident,

    /// The type the impl is for (does not include generic parameters).
    this_type: Ident,

    /// Generics including any explicit and inferred bounds for the impl.
    generics: Generics,

    /// Type has a repr(packed) attribute.
    is_packed: bool,
}

impl Parameters {
    fn new(cont: &Container) -> Self {
        Parameters {
            self_var: Ident::new("self", Span::call_site()),
            this_type: cont.ident.clone(),
            generics: build_generics(cont),
            is_packed: cont.attrs.is_packed(),
        }
    }

    /// Type name to use in error messages and `&'static str` arguments to various Serializer methods.
    fn type_name(&self) -> String {
        self.this_type.to_string()
    }
}

fn build_generics(cont: &Container) -> Generics {
    bound::with_bound(
        cont,
        cont.generics,
        needs_serialize_bound,
        &q!("_serde::Serialize"),
    )
}

const fn needs_serialize_bound(field: &attr::Field) -> bool {
    !field.skip_serializing()
}

fn serialize_body(cont: &Container, params: &Parameters) -> Fragment {
    if cont.attrs.transparent() {
        serialize_transparent(cont, params)
    } else {
        match &cont.data {
            Data::Enum(variants) => serialize_enum(params, variants, &cont.attrs),
            Data::Struct(Style::Struct, fields) => serialize_struct(params, fields, &cont.attrs),
            Data::Struct(Style::Tuple, fields) => {
                serialize_tuple_struct(params, fields, &cont.attrs)
            }
            Data::Struct(Style::Newtype, fields) => {
                serialize_newtype_struct(params, &fields[0], &cont.attrs)
            }
            Data::Struct(Style::Unit, _) => serialize_unit_struct(&cont.attrs),
        }
    }
}

fn serialize_transparent(cont: &Container, params: &Parameters) -> Fragment {
    let Data::Struct(_, fields) = &cont.data else {
        return quote_expr!("_serde::Serializer::serialize_unit(__serializer)");
    };

    let self_var = &params.self_var;
    let Some(transparent_field) = fields.iter().find(|f| f.attrs.transparent()) else {
        return quote_expr!("_serde::Serializer::serialize_unit(__serializer)");
    };
    let member = &transparent_field.member;

    let path = q_spanned!(transparent_field.span() => "_serde::Serialize::serialize");

    quote_block!(
        "#path(&#self_var.#member, __serializer)",
        path,
        self_var,
        member
    )
}

fn serialize_unit_struct(cattrs: &attr::Container) -> Fragment {
    let type_name = cattrs.name().serialize_name();

    quote_expr!(
        "_serde::Serializer::serialize_unit_struct(__serializer, #type_name)",
        type_name
    )
}

fn serialize_newtype_struct(
    params: &Parameters,
    field: &Field,
    cattrs: &attr::Container,
) -> Fragment {
    let type_name = cattrs.name().serialize_name();

    let field_expr = get_member(params, &Member::Unnamed(0));

    let func = q_spanned!(field.span() => "_serde::Serializer::serialize_newtype_struct");
    quote_expr!(
        "#func(__serializer, #type_name, #field_expr)",
        func,
        type_name,
        field_expr
    )
}

fn serialize_tuple_struct(
    params: &Parameters,
    fields: &[Field],
    cattrs: &attr::Container,
) -> Fragment {
    let serialize_stmts =
        serialize_tuple_struct_visitor(fields, params, false, &TupleTrait::SerializeTupleStruct);

    let type_name = cattrs.name().serialize_name();

    let mut serialized_fields = fields
        .iter()
        .enumerate()
        .filter(|(_, field)| !field.attrs.skip_serializing())
        .peekable();

    let let_mut = mut_if(serialized_fields.peek().is_some());

    let len = serialized_fields
        .map(|(i, field)| match field.attrs.skip_serializing_if() {
            None => q!("1"),
            Some(path) => {
                let field_expr = get_member(params, &Member::Unnamed(index_u32(i)));
                q!("if #path(#field_expr) { 0 } else { 1 }", path, field_expr)
            }
        })
        .fold(q!("0"), |sum, expr| q!("#sum + #expr", sum, expr));

    quote_block!(
        "let #let_mut __serde_state = _serde::Serializer::serialize_tuple_struct(__serializer, #type_name, #len)?;
        #serialize_stmts
        _serde::ser::SerializeTupleStruct::end(__serde_state)",
        let_mut,
        type_name,
        len,
        serialize_stmts,
    )
}

fn serialize_struct(params: &Parameters, fields: &[Field], cattrs: &attr::Container) -> Fragment {
    let has_non_skipped_flatten = fields
        .iter()
        .any(|field| field.attrs.flatten() && !field.attrs.skip_serializing());
    if has_non_skipped_flatten {
        serialize_struct_as_map(params, fields, cattrs)
    } else {
        serialize_struct_as_struct(params, fields, cattrs)
    }
}

fn serialize_struct_tag_field(cattrs: &attr::Container, struct_trait: &StructTrait) -> TokenStream {
    match cattrs.tag() {
        attr::TagType::Internal { tag } => {
            let type_name = cattrs.name().serialize_name();
            let func = struct_trait.serialize_field(Span::call_site());
            let tag = Literal::string(tag);
            q!(
                "#func(&mut __serde_state, #tag, #type_name)?;",
                func,
                tag,
                type_name
            )
        }
        _ => TokenStream::new(),
    }
}

fn serialize_struct_as_struct(
    params: &Parameters,
    fields: &[Field],
    cattrs: &attr::Container,
) -> Fragment {
    let serialize_fields =
        serialize_struct_visitor(fields, params, false, &StructTrait::SerializeStruct);

    let type_name = cattrs.name().serialize_name();

    let tag_field = serialize_struct_tag_field(cattrs, &StructTrait::SerializeStruct);
    let tag_field_exists = !tag_field.is_empty();

    let mut serialized_fields = fields
        .iter()
        .filter(|&field| !field.attrs.skip_serializing())
        .peekable();

    let let_mut = mut_if(serialized_fields.peek().is_some() || tag_field_exists);

    let len = serialized_fields
        .map(|field| match field.attrs.skip_serializing_if() {
            None => q!("1"),
            Some(path) => {
                let field_expr = get_member(params, &field.member);
                q!("if #path(#field_expr) { 0 } else { 1 }", path, field_expr)
            }
        })
        .fold(
            q!("#tag_field_exists as usize", tag_field_exists),
            |sum, expr| q!("#sum + #expr", sum, expr),
        );

    quote_block!(
        "let #let_mut __serde_state = _serde::Serializer::serialize_struct(__serializer, #type_name, #len)?;
        #tag_field
        #serialize_fields
        _serde::ser::SerializeStruct::end(__serde_state)",
        let_mut,
        type_name,
        len,
        tag_field,
        serialize_fields,
    )
}

fn serialize_struct_as_map(
    params: &Parameters,
    fields: &[Field],
    cattrs: &attr::Container,
) -> Fragment {
    let serialize_fields =
        serialize_struct_visitor(fields, params, false, &StructTrait::SerializeMap);

    let tag_field = serialize_struct_tag_field(cattrs, &StructTrait::SerializeMap);
    let tag_field_exists = !tag_field.is_empty();

    let mut serialized_fields = fields
        .iter()
        .filter(|&field| !field.attrs.skip_serializing())
        .peekable();

    let let_mut = mut_if(serialized_fields.peek().is_some() || tag_field_exists);

    quote_block!(
        "let #let_mut __serde_state = _serde::Serializer::serialize_map(__serializer, _serde::__private::None)?;
        #tag_field
        #serialize_fields
        _serde::ser::SerializeMap::end(__serde_state)",
        let_mut,
        tag_field,
        serialize_fields,
    )
}

fn serialize_enum(params: &Parameters, variants: &[Variant], cattrs: &attr::Container) -> Fragment {
    let self_var = &params.self_var;

    let arms: Vec<TokenStream> = variants
        .iter()
        .enumerate()
        .map(|(variant_index, variant)| {
            serialize_variant(params, variant, index_u32(variant_index), cattrs)
        })
        .collect();

    quote_expr!(
        "match *#self_var {
            #arms
        }",
        self_var,
        arms
    )
}

fn serialize_variant(
    params: &Parameters,
    variant: &Variant,
    variant_index: u32,
    cattrs: &attr::Container,
) -> TokenStream {
    let this_value = &params.this_type;
    let variant_ident = &variant.ident;

    let case = match variant.style {
        Style::Unit => q!("#this_value::#variant_ident", this_value, variant_ident),
        Style::Newtype => q!(
            "#this_value::#variant_ident(ref __field0)",
            this_value,
            variant_ident
        ),
        Style::Tuple => {
            let field_names: Vec<TokenStream> = (0..variant.fields.len())
                .map(|i| {
                    let name = field_ident(i);
                    q!("ref #name", name)
                })
                .collect();
            let field_names = aterm_macro_support::join(&field_names, ',', false);
            q!(
                "#this_value::#variant_ident(#field_names)",
                this_value,
                variant_ident,
                field_names
            )
        }
        Style::Struct => {
            let members: Vec<TokenStream> = variant
                .fields
                .iter()
                .map(|f| {
                    let member = &f.member;
                    q!("ref #member", member)
                })
                .collect();
            let members = aterm_macro_support::join(&members, ',', false);
            q!(
                "#this_value::#variant_ident { #members }",
                this_value,
                variant_ident,
                members
            )
        }
    };

    let body = Match(match cattrs.tag() {
        attr::TagType::External => {
            serialize_externally_tagged_variant(params, variant, variant_index, cattrs)
        }
        attr::TagType::Internal { tag } => {
            serialize_internally_tagged_variant(params, variant, cattrs, tag)
        }
        attr::TagType::None => serialize_untagged_variant(params, variant, cattrs),
    });

    q!("#case => #body", case, body)
}

fn serialize_externally_tagged_variant(
    params: &Parameters,
    variant: &Variant,
    variant_index: u32,
    cattrs: &attr::Container,
) -> Fragment {
    let type_name = cattrs.name().serialize_name();
    let variant_name = variant.attrs.name().serialize_name();
    let variant_index_lit = Literal::u32_suffixed(variant_index);

    match effective_style(variant) {
        Style::Unit => quote_expr!(
            "_serde::Serializer::serialize_unit_variant(
                __serializer,
                #type_name,
                #variant_index,
                #variant_name,
            )",
            type_name,
            variant_index = variant_index_lit,
            variant_name,
        ),
        Style::Newtype => {
            let field = &variant.fields[0];
            let field_expr = field_ident(0);

            let func = q_spanned!(field.span() => "_serde::Serializer::serialize_newtype_variant");
            quote_expr!(
                "#func(
                    __serializer,
                    #type_name,
                    #variant_index,
                    #variant_name,
                    #field_expr,
                )",
                func,
                type_name,
                variant_index = variant_index_lit,
                variant_name,
                field_expr,
            )
        }
        Style::Tuple => serialize_tuple_variant(
            &TupleVariant::ExternallyTagged {
                type_name,
                variant_index,
                variant_name,
            },
            params,
            &variant.fields,
        ),
        Style::Struct => serialize_struct_variant(
            &StructVariant::ExternallyTagged {
                variant_index,
                variant_name,
            },
            params,
            &variant.fields,
            type_name,
        ),
    }
}

fn serialize_internally_tagged_variant(
    params: &Parameters,
    variant: &Variant,
    cattrs: &attr::Container,
    tag: &str,
) -> Fragment {
    let type_name = cattrs.name().serialize_name();
    let variant_name = variant.attrs.name().serialize_name();

    let enum_ident_str = Literal::string(&params.type_name());
    let variant_ident_str = Literal::string(&variant.ident.to_string());
    let tag_lit = Literal::string(tag);

    match effective_style(variant) {
        Style::Unit => quote_block!(
            "let mut __struct = _serde::Serializer::serialize_struct(
                __serializer, #type_name, 1)?;
            _serde::ser::SerializeStruct::serialize_field(
                &mut __struct, #tag, #variant_name)?;
            _serde::ser::SerializeStruct::end(__struct)",
            type_name,
            tag = tag_lit,
            variant_name,
        ),
        Style::Newtype => {
            let field = &variant.fields[0];
            let field_expr = field_ident(0);

            let func =
                q_spanned!(field.span() => "_serde::__private::ser::serialize_tagged_newtype");
            quote_expr!(
                "#func(
                    __serializer,
                    #enum_ident_str,
                    #variant_ident_str,
                    #tag,
                    #variant_name,
                    #field_expr,
                )",
                func,
                enum_ident_str,
                variant_ident_str,
                tag = tag_lit,
                variant_name,
                field_expr,
            )
        }
        Style::Struct => serialize_struct_variant(
            &StructVariant::InternallyTagged { tag, variant_name },
            params,
            &variant.fields,
            type_name,
        ),
        Style::Tuple => quote_expr!(
            "_serde::__private::Err(_serde::ser::Error::custom(\"tuple variant in an internally tagged enum\"))"
        ),
    }
}

fn serialize_untagged_variant(
    params: &Parameters,
    variant: &Variant,
    cattrs: &attr::Container,
) -> Fragment {
    match effective_style(variant) {
        Style::Unit => quote_expr!("_serde::Serializer::serialize_unit(__serializer)"),
        Style::Newtype => {
            let field = &variant.fields[0];
            let field_expr = field_ident(0);

            let func = q_spanned!(field.span() => "_serde::Serialize::serialize");
            quote_expr!("#func(#field_expr, __serializer)", func, field_expr)
        }
        Style::Tuple => serialize_tuple_variant(&TupleVariant::Untagged, params, &variant.fields),
        Style::Struct => {
            let type_name = cattrs.name().serialize_name();
            serialize_struct_variant(&StructVariant::Untagged, params, &variant.fields, type_name)
        }
    }
}

enum TupleVariant<'a> {
    ExternallyTagged {
        type_name: &'a Name,
        variant_index: u32,
        variant_name: &'a Name,
    },
    Untagged,
}

fn serialize_tuple_variant(
    context: &TupleVariant,
    params: &Parameters,
    fields: &[Field],
) -> Fragment {
    let tuple_trait = match context {
        TupleVariant::ExternallyTagged { .. } => TupleTrait::SerializeTupleVariant,
        TupleVariant::Untagged => TupleTrait::SerializeTuple,
    };

    let serialize_stmts = serialize_tuple_struct_visitor(fields, params, true, &tuple_trait);

    let mut serialized_fields = fields
        .iter()
        .enumerate()
        .filter(|(_, field)| !field.attrs.skip_serializing())
        .peekable();

    let let_mut = mut_if(serialized_fields.peek().is_some());

    let len = serialized_fields
        .map(|(i, field)| match field.attrs.skip_serializing_if() {
            None => q!("1"),
            Some(path) => {
                let field_expr = field_ident(i);
                q!("if #path(#field_expr) { 0 } else { 1 }", path, field_expr)
            }
        })
        .fold(q!("0"), |sum, expr| q!("#sum + #expr", sum, expr));

    match context {
        TupleVariant::ExternallyTagged {
            type_name,
            variant_index,
            variant_name,
        } => quote_block!(
            "let #let_mut __serde_state = _serde::Serializer::serialize_tuple_variant(
                __serializer,
                #type_name,
                #variant_index,
                #variant_name,
                #len)?;
            #serialize_stmts
            _serde::ser::SerializeTupleVariant::end(__serde_state)",
            let_mut,
            type_name,
            variant_index = Literal::u32_suffixed(*variant_index),
            variant_name,
            len,
            serialize_stmts,
        ),
        TupleVariant::Untagged => quote_block!(
            "let #let_mut __serde_state = _serde::Serializer::serialize_tuple(
                __serializer,
                #len)?;
            #serialize_stmts
            _serde::ser::SerializeTuple::end(__serde_state)",
            let_mut,
            len,
            serialize_stmts,
        ),
    }
}

enum StructVariant<'a> {
    ExternallyTagged {
        variant_index: u32,
        variant_name: &'a Name,
    },
    InternallyTagged {
        tag: &'a str,
        variant_name: &'a Name,
    },
    Untagged,
}

fn serialize_struct_variant(
    context: &StructVariant,
    params: &Parameters,
    fields: &[Field],
    name: &Name,
) -> Fragment {
    if fields.iter().any(|field| field.attrs.flatten()) {
        return serialize_struct_variant_with_flatten(context, params, fields, name);
    }

    let struct_trait = match context {
        StructVariant::ExternallyTagged { .. } => StructTrait::SerializeStructVariant,
        StructVariant::InternallyTagged { .. } | StructVariant::Untagged => {
            StructTrait::SerializeStruct
        }
    };

    let serialize_fields = serialize_struct_visitor(fields, params, true, &struct_trait);

    let mut serialized_fields = fields
        .iter()
        .filter(|&field| !field.attrs.skip_serializing())
        .peekable();

    let let_mut = mut_if(serialized_fields.peek().is_some());

    let len = serialized_fields
        .map(|field| {
            let member = &field.member;

            match field.attrs.skip_serializing_if() {
                Some(path) => q!("if #path(#member) { 0 } else { 1 }", path, member),
                None => q!("1"),
            }
        })
        .fold(q!("0"), |sum, expr| q!("#sum + #expr", sum, expr));

    match context {
        StructVariant::ExternallyTagged {
            variant_index,
            variant_name,
        } => quote_block!(
            "let #let_mut __serde_state = _serde::Serializer::serialize_struct_variant(
                __serializer,
                #name,
                #variant_index,
                #variant_name,
                #len,
            )?;
            #serialize_fields
            _serde::ser::SerializeStructVariant::end(__serde_state)",
            let_mut,
            name,
            variant_index = Literal::u32_suffixed(*variant_index),
            variant_name,
            len,
            serialize_fields,
        ),
        StructVariant::InternallyTagged { tag, variant_name } => quote_block!(
            "let mut __serde_state = _serde::Serializer::serialize_struct(
                __serializer,
                #name,
                #len + 1,
            )?;
            _serde::ser::SerializeStruct::serialize_field(
                &mut __serde_state,
                #tag,
                #variant_name,
            )?;
            #serialize_fields
            _serde::ser::SerializeStruct::end(__serde_state)",
            name,
            len,
            tag = Literal::string(tag),
            variant_name,
            serialize_fields,
        ),
        StructVariant::Untagged => quote_block!(
            "let #let_mut __serde_state = _serde::Serializer::serialize_struct(
                __serializer,
                #name,
                #len,
            )?;
            #serialize_fields
            _serde::ser::SerializeStruct::end(__serde_state)",
            let_mut,
            name,
            len,
            serialize_fields,
        ),
    }
}

fn serialize_struct_variant_with_flatten(
    context: &StructVariant,
    params: &Parameters,
    fields: &[Field],
    name: &Name,
) -> Fragment {
    let struct_trait = StructTrait::SerializeMap;
    let serialize_fields = serialize_struct_visitor(fields, params, true, &struct_trait);

    let mut serialized_fields = fields
        .iter()
        .filter(|&field| !field.attrs.skip_serializing())
        .peekable();

    let let_mut = mut_if(serialized_fields.peek().is_some());

    match context {
        StructVariant::ExternallyTagged {
            variant_index,
            variant_name,
        } => {
            let this_type = &params.this_type;
            let fields_ty: Vec<TokenStream> = fields
                .iter()
                .map(|f| {
                    let ty = &f.ty;
                    q!("&'__a #ty,", ty)
                })
                .collect();
            let members: Vec<TokenStream> = fields
                .iter()
                .map(|f| {
                    let member = &f.member;
                    q!("#member,", member)
                })
                .collect();

            let ty_generics = params.generics.ty_generics();
            let where_clause = params.generics.where_clause();
            let wrapper_generics = bound::with_lifetime_bound(&params.generics, "__a");
            let wrapper_impl_generics = wrapper_generics.impl_generics();
            let wrapper_ty_generics = wrapper_generics.ty_generics();

            quote_block!(
                "#[doc(hidden)]
                struct __EnumFlatten #wrapper_impl_generics #where_clause {
                    data: (#fields_ty),
                    phantom: _serde::__private::PhantomData<#this_type #ty_generics>,
                }

                #[automatically_derived]
                impl #wrapper_impl_generics _serde::Serialize for __EnumFlatten #wrapper_ty_generics #where_clause {
                    fn serialize<__S>(&self, __serializer: __S) -> _serde::__private::Result<__S::Ok, __S::Error>
                    where
                        __S: _serde::Serializer,
                    {
                        let (#members) = self.data;
                        let #let_mut __serde_state = _serde::Serializer::serialize_map(
                            __serializer,
                            _serde::__private::None)?;
                        #serialize_fields
                        _serde::ser::SerializeMap::end(__serde_state)
                    }
                }

                _serde::Serializer::serialize_newtype_variant(
                    __serializer,
                    #name,
                    #variant_index,
                    #variant_name,
                    &__EnumFlatten {
                        data: (#members),
                        phantom: _serde::__private::PhantomData::<#this_type #ty_generics>,
                    })",
                wrapper_impl_generics,
                where_clause,
                fields_ty,
                this_type,
                ty_generics,
                wrapper_ty_generics,
                members,
                let_mut,
                serialize_fields,
                name,
                variant_index = Literal::u32_suffixed(*variant_index),
                variant_name,
            )
        }
        StructVariant::InternallyTagged { tag, variant_name } => quote_block!(
            "let #let_mut __serde_state = _serde::Serializer::serialize_map(
                __serializer,
                _serde::__private::None)?;
            _serde::ser::SerializeMap::serialize_entry(
                &mut __serde_state,
                #tag,
                #variant_name,
            )?;
            #serialize_fields
            _serde::ser::SerializeMap::end(__serde_state)",
            let_mut,
            tag = Literal::string(tag),
            variant_name,
            serialize_fields,
        ),
        StructVariant::Untagged => quote_block!(
            "let #let_mut __serde_state = _serde::Serializer::serialize_map(
                __serializer,
                _serde::__private::None)?;
            #serialize_fields
            _serde::ser::SerializeMap::end(__serde_state)",
            let_mut,
            serialize_fields,
        ),
    }
}

fn serialize_tuple_struct_visitor(
    fields: &[Field],
    params: &Parameters,
    is_enum: bool,
    tuple_trait: &TupleTrait,
) -> Vec<TokenStream> {
    fields
        .iter()
        .enumerate()
        .filter(|(_, field)| !field.attrs.skip_serializing())
        .map(|(i, field)| {
            let field_expr = if is_enum {
                field_ident(i).to_token_stream()
            } else {
                get_member(params, &Member::Unnamed(index_u32(i)))
            };

            let skip = field
                .attrs
                .skip_serializing_if()
                .map(|path| q!("#path(#field_expr)", path, field_expr));

            let func = tuple_trait.serialize_element(field.span());
            let ser = q!("#func(&mut __serde_state, #field_expr)?;", func, field_expr);

            match skip {
                None => ser,
                Some(skip) => q!("if !#skip { #ser }", skip, ser),
            }
        })
        .collect()
}

fn serialize_struct_visitor(
    fields: &[Field],
    params: &Parameters,
    is_enum: bool,
    struct_trait: &StructTrait,
) -> Vec<TokenStream> {
    fields
        .iter()
        .filter(|&field| !field.attrs.skip_serializing())
        .map(|field| {
            let member = &field.member;

            let field_expr = if is_enum {
                member.to_token_stream()
            } else {
                get_member(params, member)
            };

            let key_expr = field.attrs.name().serialize_name();

            let skip = field
                .attrs
                .skip_serializing_if()
                .map(|path| q!("#path(#field_expr)", path, field_expr));

            let span = field.span();
            let ser = if field.attrs.flatten() {
                let func = q_spanned!(span => "_serde::Serialize::serialize");
                q!(
                    "#func(&#field_expr, _serde::__private::ser::FlatMapSerializer(&mut __serde_state))?;",
                    func,
                    field_expr
                )
            } else {
                let func = struct_trait.serialize_field(span);
                q!(
                    "#func(&mut __serde_state, #key_expr, #field_expr)?;",
                    func,
                    key_expr,
                    field_expr
                )
            };

            match skip {
                None => ser,
                Some(skip) => {
                    if let Some(skip_func) = struct_trait.skip_field(span) {
                        q!(
                            "if !#skip {
                                #ser
                            } else {
                                #skip_func(&mut __serde_state, #key_expr)?;
                            }",
                            skip,
                            ser,
                            skip_func,
                            key_expr
                        )
                    } else {
                        q!(
                            "if !#skip {
                                #ser
                            }",
                            skip,
                            ser
                        )
                    }
                }
            }
        })
        .collect()
}

fn mut_if(is_mut: bool) -> Option<TokenStream> {
    if is_mut { Some(q!("mut")) } else { None }
}

fn get_member(params: &Parameters, member: &Member) -> TokenStream {
    let self_var = &params.self_var;
    if params.is_packed {
        q!("&{#self_var.#member}", self_var, member)
    } else {
        q!("&#self_var.#member", self_var, member)
    }
}

fn effective_style(variant: &Variant) -> Style {
    match variant.style {
        Style::Newtype if variant.fields[0].attrs.skip_serializing() => Style::Unit,
        other => other,
    }
}

/// `__field{i}`, the binding of a tuple variant's `i`th field.
fn field_ident(i: usize) -> Ident {
    Ident::new(&format!("__field{i}"), Span::call_site())
}

fn index_u32(i: usize) -> u32 {
    u32::try_from(i).unwrap_or(u32::MAX)
}

enum StructTrait {
    SerializeMap,
    SerializeStruct,
    SerializeStructVariant,
}

impl StructTrait {
    fn serialize_field(&self, span: Span) -> TokenStream {
        match *self {
            StructTrait::SerializeMap => {
                q_spanned!(span => "_serde::ser::SerializeMap::serialize_field_entry")
            }
            StructTrait::SerializeStruct => {
                q_spanned!(span => "_serde::ser::SerializeStruct::serialize_field")
            }
            StructTrait::SerializeStructVariant => {
                q_spanned!(span => "_serde::ser::SerializeStructVariant::serialize_field")
            }
        }
    }

    fn skip_field(&self, span: Span) -> Option<TokenStream> {
        match *self {
            StructTrait::SerializeMap => None,
            StructTrait::SerializeStruct => {
                Some(q_spanned!(span => "_serde::ser::SerializeStruct::skip_field"))
            }
            StructTrait::SerializeStructVariant => {
                Some(q_spanned!(span => "_serde::ser::SerializeStructVariant::skip_field"))
            }
        }
    }
}

enum TupleTrait {
    SerializeTuple,
    SerializeTupleStruct,
    SerializeTupleVariant,
}

impl TupleTrait {
    fn serialize_element(&self, span: Span) -> TokenStream {
        match *self {
            TupleTrait::SerializeTuple => {
                q_spanned!(span => "_serde::ser::SerializeTuple::serialize_element")
            }
            TupleTrait::SerializeTupleStruct => {
                q_spanned!(span => "_serde::ser::SerializeTupleStruct::serialize_field")
            }
            TupleTrait::SerializeTupleVariant => {
                q_spanned!(span => "_serde::ser::SerializeTupleVariant::serialize_field")
            }
        }
    }
}
