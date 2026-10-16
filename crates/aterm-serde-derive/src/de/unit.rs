// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde_derive 1.0.228 src/de/unit.rs and modified by the aterm
// project in 2026 (templates over std proc_macro; no `expecting`).
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

use aterm_macro_support::q;

use crate::attr;
use crate::de::{Parameters, visitor_expr, visitor_impl, visitor_struct};
use crate::fragment::Fragment;

/// Generates `Deserialize::deserialize` body for a `struct Unit;`
pub(super) fn deserialize(params: &Parameters, cattrs: &attr::Container) -> Fragment {
    let this_value = &params.this_type;
    let type_name = cattrs.name().deserialize_name();

    let expecting = format!("unit struct {}", params.type_name());

    let visitor_struct = visitor_struct(params);
    let visitor_impl = visitor_impl(
        params,
        &expecting,
        &q!(
            "#[inline]
            fn visit_unit<__E>(self) -> _serde::__private::Result<Self::Value, __E>
            where
                __E: _serde::de::Error,
            {
                _serde::__private::Ok(#this_value)
            }",
            this_value
        ),
    );
    let visitor_expr = visitor_expr(params);

    quote_block!(
        "#visitor_struct

        #visitor_impl

        _serde::Deserializer::deserialize_unit_struct(
            __deserializer,
            #type_name,
            #visitor_expr,
        )",
        visitor_struct,
        visitor_impl,
        type_name,
        visitor_expr,
    )
}
