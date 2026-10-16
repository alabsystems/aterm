// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde 1.0.228 test_suite/tests/test_value.rs (git tag v1.0.228) and
// modified by the aterm project in 2026: ported onto aterm-serde's surface.
// See ../LICENSE-MIT and ../LICENSE-APACHE for the upstream licenses.

#![allow(clippy::derive_partial_eq_without_eq, clippy::similar_names)]

use crate::support::{Token, assert_de_tokens};
use aterm_serde::de::value::{self, MapAccessDeserializer};
use aterm_serde::de::{Deserialize, Deserializer, IntoDeserializer, MapAccess, Visitor};
use serde_derive::Deserialize;
use std::fmt;

#[test]
fn test_u32_to_enum() {
    #[derive(Deserialize, Debug, PartialEq)]
    enum E {
        A,
        B,
    }

    let deserializer = IntoDeserializer::<value::Error>::into_deserializer(1u32);
    let e: E = E::deserialize(deserializer).unwrap();
    assert_eq!(E::B, e);
}

#[test]
fn test_map_access_to_enum() {
    #[derive(PartialEq, Debug)]
    struct Potential(PotentialKind);

    #[derive(PartialEq, Debug, Deserialize)]
    enum PotentialKind {
        Airebo(Airebo),
    }

    #[derive(PartialEq, Debug, Deserialize)]
    struct Airebo {
        lj_sigma: f64,
    }

    impl<'de> Deserialize<'de> for Potential {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            struct PotentialVisitor;

            impl<'de> Visitor<'de> for PotentialVisitor {
                type Value = Potential;

                fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                    formatter.write_str("a map")
                }

                fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
                where
                    A: MapAccess<'de>,
                {
                    Deserialize::deserialize(MapAccessDeserializer::new(map)).map(Potential)
                }
            }

            deserializer.deserialize_any(PotentialVisitor)
        }
    }

    let expected = Potential(PotentialKind::Airebo(Airebo { lj_sigma: 14.0 }));

    assert_de_tokens(
        &expected,
        &[
            Token::Map { len: Some(1) },
            Token::Str("Airebo"),
            Token::Map { len: Some(1) },
            Token::Str("lj_sigma"),
            Token::F64(14.0),
            Token::MapEnd,
            Token::MapEnd,
        ],
    );
}
