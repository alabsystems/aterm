// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde_derive 1.0.228 src/bound.rs and modified by the aterm
// project in 2026: the type walk reads aterm-macro-support's `Ty`, and the
// `bound` attribute (outside aterm's set) is gone.
// See ../LICENSE-MIT and ../LICENSE-APACHE for the upstream licenses.

use std::collections::HashSet;

use aterm_macro_support::{
    Bound, Errors, GenericArg, GenericParam, Generics, Lifetime, PathArgs, ToTokens, Ty, TyPath,
};
use proc_macro::{Punct, Spacing, TokenStream, TokenTree};

use crate::ast::{Container, Data};
use crate::attr;

fn punct(ch: char) -> TokenTree {
    TokenTree::Punct(Punct::new(ch, Spacing::Alone))
}

/// `bounded: bound` as one where predicate.
fn predicate(bounded: &dyn ToTokens, bound: &TokenStream) -> TokenStream {
    let mut out = Vec::new();
    bounded.to_tokens(&mut out);
    out.push(punct(':'));
    out.extend(bound.clone());
    out.into_iter().collect()
}

// Puts the given bound on any generic type parameters that are used in fields
// for which filter returns true.
//
// For example, the following struct needs the bound `A: Serialize, B:
// Serialize`.
//
//     struct S<'b, A, B: 'b, C> {
//         a: A,
//         b: Option<&'b B>
//         #[serde(skip_serializing)]
//         c: C,
//     }
pub fn with_bound(
    cont: &Container,
    generics: &Generics,
    filter: fn(&attr::Field) -> bool,
    bound: &TokenStream,
) -> Generics {
    if generics.type_params().next().is_none() {
        return generics.clone();
    }

    struct FindTyParams {
        // Set of all generic type parameters on the current struct (A, B, C in
        // the example). Initialized up front.
        all_type_params: HashSet<String>,

        // Set of generic type parameters used in fields for which filter
        // returns true (A and B in the example). Filled in as the visitor sees
        // them.
        relevant_type_params: HashSet<String>,

        // Fields whose type is an associated type of one of the generic type
        // parameters.
        associated_type_usage: Vec<TokenStream>,
    }

    impl FindTyParams {
        fn visit_field(&mut self, field: &crate::ast::Field) {
            if let Ty::Path(ty) = attr::ungroup(&field.ty_tree)
                && ty.segments.len() > 1
                && self
                    .all_type_params
                    .contains(&ty.segments[0].ident.to_string())
            {
                self.associated_type_usage.push(ungroup_tokens(&field.ty));
            }
            self.visit_type(&field.ty_tree);
        }

        fn visit_path(&mut self, path: &TyPath) {
            if let Some(seg) = path.segments.last()
                && seg.ident.to_string() == "PhantomData"
            {
                // Hardcoded exception, because PhantomData<T> implements
                // Serialize and Deserialize whether or not T implements it.
                return;
            }
            if !path.leading_colon && path.segments.len() == 1 {
                let id = path.segments[0].ident.to_string();
                if self.all_type_params.contains(&id) {
                    self.relevant_type_params.insert(id);
                }
            }
            for segment in &path.segments {
                self.visit_path_arguments(&segment.args);
            }
        }

        // Everything below is simply traversing the syntax tree.

        fn visit_type(&mut self, ty: &Ty) {
            match ty {
                Ty::Array(elem)
                | Ty::Group(elem)
                | Ty::Paren(elem)
                | Ty::Ptr(elem)
                | Ty::Slice(elem)
                | Ty::Reference { elem, .. } => self.visit_type(elem),
                Ty::BareFn { inputs, output } => {
                    for arg in inputs {
                        self.visit_type(arg);
                    }
                    if let Some(output) = output {
                        self.visit_type(output);
                    }
                }
                Ty::ImplTrait(bounds) | Ty::TraitObject(bounds) => {
                    for bound in bounds {
                        if let Bound::Trait(path) = bound {
                            self.visit_path(path);
                        }
                    }
                }
                Ty::Path(ty) => {
                    if let Some(qself) = &ty.qself {
                        self.visit_type(qself);
                    }
                    self.visit_path(ty);
                }
                Ty::Tuple(elems) => {
                    for elem in elems {
                        self.visit_type(elem);
                    }
                }
                // Type parameter should not be considered used by a macro path.
                //
                //     struct TypeMacro<T> {
                //         mac: T!(),
                //         marker: PhantomData<T>,
                //     }
                Ty::Macro(_) | Ty::Infer | Ty::Never | Ty::Other => {}
            }
        }

        fn visit_path_arguments(&mut self, arguments: &PathArgs) {
            match arguments {
                PathArgs::None => {}
                PathArgs::Angle(arguments) => {
                    for arg in arguments {
                        match arg {
                            GenericArg::Type(arg) | GenericArg::AssocType(_, arg) => {
                                self.visit_type(arg);
                            }
                            GenericArg::Lifetime(_) | GenericArg::Other => {}
                        }
                    }
                }
                PathArgs::Paren { inputs, output } => {
                    for argument in inputs {
                        self.visit_type(argument);
                    }
                    if let Some(output) = output {
                        self.visit_type(output);
                    }
                }
            }
        }
    }

    let all_type_params = generics.type_params().map(ToString::to_string).collect();

    let mut visitor = FindTyParams {
        all_type_params,
        relevant_type_params: HashSet::new(),
        associated_type_usage: Vec::new(),
    };
    match &cont.data {
        Data::Enum(variants) => {
            for variant in variants {
                let relevant_fields = variant.fields.iter().filter(|field| filter(&field.attrs));
                for field in relevant_fields {
                    visitor.visit_field(field);
                }
            }
        }
        Data::Struct(_, fields) => {
            for field in fields.iter().filter(|field| filter(&field.attrs)) {
                visitor.visit_field(field);
            }
        }
    }

    let relevant_type_params = visitor.relevant_type_params;
    let associated_type_usage = visitor.associated_type_usage;
    let mut generics = generics.clone();
    let params: Vec<_> = generics
        .type_params()
        .filter(|id| relevant_type_params.contains(&id.to_string()))
        .cloned()
        .collect();
    for id in params {
        generics.where_predicates.push(predicate(&id, bound));
    }
    for ty in associated_type_usage {
        generics.where_predicates.push(predicate(&ty, bound));
    }
    generics
}

/// `tokens` with the None-delimited groups that wrap all of it removed.
fn ungroup_tokens(tokens: &TokenStream) -> TokenStream {
    let mut tokens = tokens.clone();
    loop {
        let mut iter = tokens.clone().into_iter();
        match (iter.next(), iter.next()) {
            (Some(TokenTree::Group(g)), None) if g.delimiter() == proc_macro::Delimiter::None => {
                tokens = g.stream();
            }
            _ => return tokens,
        }
    }
}

pub fn with_self_bound(
    cx: &Errors,
    cont: &Container,
    generics: &Generics,
    bound: &TokenStream,
) -> Generics {
    let mut generics = generics.clone();
    if let Some(ty) = type_of_item(cx, cont) {
        generics.where_predicates.push(predicate(&ty, bound));
    }
    generics
}

pub fn with_lifetime_bound(generics: &Generics, lifetime: &str) -> Generics {
    let bound = Lifetime::new(lifetime, proc_macro::Span::call_site());
    let def = GenericParam::Lifetime {
        attrs: Vec::new(),
        lifetime: bound.clone(),
        bounds: Vec::new(),
    };

    let params = Some(def)
        .into_iter()
        .chain(generics.params.iter().cloned().map(|mut param| {
            match &mut param {
                GenericParam::Lifetime { bounds, .. } => bounds.push(bound.clone()),
                GenericParam::Type { bounds, .. } => bounds.push(bound.to_token_stream()),
                GenericParam::Const { .. } => {}
            }
            param
        }))
        .collect();

    Generics {
        params,
        where_predicates: generics.where_predicates.clone(),
    }
}

/// `Ident<params as arguments>`, upstream's spelling of the item's own type in
/// a bound (`<>` when there are no parameters).
fn type_of_item(cx: &Errors, cont: &Container) -> Option<TokenStream> {
    let mut out = vec![TokenTree::Ident(cont.ident.clone()), punct('<')];
    for (i, param) in cont.generics.params.iter().enumerate() {
        if i > 0 {
            out.push(punct(','));
        }
        match param {
            GenericParam::Type { ident, .. } => out.push(TokenTree::Ident(ident.clone())),
            GenericParam::Lifetime { lifetime, .. } => lifetime.to_tokens(&mut out),
            GenericParam::Const { ident, .. } => {
                cx.at(
                    ident.span(),
                    "Serde does not support const generics with #[serde(default)] yet",
                );
                return None;
            }
        }
    }
    out.push(punct('>'));
    Some(out.into_iter().collect())
}
