// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde_derive 1.0.228 src/de/enum_.rs and modified by the aterm
// project in 2026 (templates over std proc_macro; no variant-level
// `untagged`, `skip` or `other`, and no adjacent tagging).
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

use aterm_macro_support::q;
use proc_macro::TokenStream;

use crate::ast::Variant;
use crate::attr;
use crate::de::enum_externally;
use crate::de::enum_internally;
use crate::de::enum_untagged;
use crate::de::identifier;
use crate::de::{FieldWithAliases, Parameters, field_i};
use crate::fragment::{Fragment, Stmts};

/// Generates `Deserialize::deserialize` body for an `enum Enum {...}`
pub(super) fn deserialize(
    params: &Parameters,
    variants: &[Variant],
    cattrs: &attr::Container,
) -> Fragment {
    match cattrs.tag() {
        attr::TagType::External => enum_externally::deserialize(params, variants, cattrs),
        attr::TagType::Internal { tag } => {
            enum_internally::deserialize(params, variants, cattrs, tag)
        }
        attr::TagType::None => enum_untagged::deserialize(params, variants, cattrs),
    }
}

pub fn prepare_enum_variant_enum(variants: &[Variant]) -> (TokenStream, Stmts) {
    let variant_names: Vec<&attr::Name> = variants
        .iter()
        .flat_map(|variant| variant.attrs.aliases())
        .collect();
    let variant_names = aterm_macro_support::join(&variant_names, ',', false);
    let variants_stmt = q!(
        "#[doc(hidden)]
        const VARIANTS: &'static [&'static str] = &[ #variant_names ];",
        variant_names
    );

    let deserialized_variants: Vec<_> = variants
        .iter()
        .enumerate()
        .map(|(i, variant)| FieldWithAliases {
            ident: field_i(i),
            aliases: variant.attrs.aliases(),
        })
        .collect();

    let variant_visitor = Stmts(identifier::deserialize_generated(
        &deserialized_variants,
        false, // variant identifiers do not depend on the presence of flatten fields
        true,
        None,
        None,
    ));

    (variants_stmt, variant_visitor)
}
