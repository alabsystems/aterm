// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Compile-fail controls for the closed attribute set
//! (docs/AUDIT-third-party-2026-10-15.md, SP.5).
//!
//! Each refused form below is a `compile_fail` doctest after an accepted twin
//! that differs from it only in the attribute, so a refusal cannot pass
//! because its snippet was broken some other way. Every refused form except
//! the unknown keys and the union is one registry serde_derive 1.0.228
//! compiles: the error is this derive's, by design.
//!
//! # Fields
//!
//! Accepted twin:
//!
//! ```
//! # extern crate aterm_serde as serde;
//! # use aterm_serde_derive::{Deserialize, Serialize};
//! # #[allow(dead_code)]
//! # mod m {
//! #     pub fn serialize<S: serde::Serializer>(v: &u8, s: S) -> Result<S::Ok, S::Error> {
//! #         serde::Serialize::serialize(v, s)
//! #     }
//! #     pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u8, D::Error> {
//! #         serde::Deserialize::deserialize(d)
//! #     }
//! # }
//! #[derive(Serialize, Deserialize)]
//! struct S {
//!     #[serde(default)]
//!     a: u8,
//! }
//! # fn main() {}
//! ```
//!
//! `with` (upstream's, outside the set):
//!
//! ```compile_fail
//! # extern crate aterm_serde as serde;
//! # use aterm_serde_derive::{Deserialize, Serialize};
//! # #[allow(dead_code)]
//! # mod m {
//! #     pub fn serialize<S: serde::Serializer>(v: &u8, s: S) -> Result<S::Ok, S::Error> {
//! #         serde::Serialize::serialize(v, s)
//! #     }
//! #     pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u8, D::Error> {
//! #         serde::Deserialize::deserialize(d)
//! #     }
//! # }
//! #[derive(Serialize, Deserialize)]
//! struct S {
//!     #[serde(with = "m")]
//!     a: u8,
//! }
//! # fn main() {}
//! ```
//!
//! `serialize_with` (upstream's, outside the set):
//!
//! ```compile_fail
//! # extern crate aterm_serde as serde;
//! # use aterm_serde_derive::{Deserialize, Serialize};
//! # #[allow(dead_code)]
//! # mod m {
//! #     pub fn serialize<S: serde::Serializer>(v: &u8, s: S) -> Result<S::Ok, S::Error> {
//! #         serde::Serialize::serialize(v, s)
//! #     }
//! #     pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u8, D::Error> {
//! #         serde::Deserialize::deserialize(d)
//! #     }
//! # }
//! #[derive(Serialize, Deserialize)]
//! struct S {
//!     #[serde(serialize_with = "m::serialize")]
//!     a: u8,
//! }
//! # fn main() {}
//! ```
//!
//! `skip_serializing` (upstream's one-direction skip, outside the set):
//!
//! ```compile_fail
//! # extern crate aterm_serde as serde;
//! # use aterm_serde_derive::{Deserialize, Serialize};
//! # #[allow(dead_code)]
//! # mod m {
//! #     pub fn serialize<S: serde::Serializer>(v: &u8, s: S) -> Result<S::Ok, S::Error> {
//! #         serde::Serialize::serialize(v, s)
//! #     }
//! #     pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u8, D::Error> {
//! #         serde::Deserialize::deserialize(d)
//! #     }
//! # }
//! #[derive(Serialize, Deserialize)]
//! struct S {
//!     #[serde(skip_serializing)]
//!     a: u8,
//! }
//! # fn main() {}
//! ```
//!
//! `rename(...)`, the list form of a key in the set:
//!
//! ```compile_fail
//! # extern crate aterm_serde as serde;
//! # use aterm_serde_derive::{Deserialize, Serialize};
//! # #[allow(dead_code)]
//! # mod m {
//! #     pub fn serialize<S: serde::Serializer>(v: &u8, s: S) -> Result<S::Ok, S::Error> {
//! #         serde::Serialize::serialize(v, s)
//! #     }
//! #     pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u8, D::Error> {
//! #         serde::Deserialize::deserialize(d)
//! #     }
//! # }
//! #[derive(Serialize, Deserialize)]
//! struct S {
//!     #[serde(rename(serialize = "b"))]
//!     a: u8,
//! }
//! # fn main() {}
//! ```
//!
//! An unknown key:
//!
//! ```compile_fail
//! # extern crate aterm_serde as serde;
//! # use aterm_serde_derive::{Deserialize, Serialize};
//! # #[allow(dead_code)]
//! # mod m {
//! #     pub fn serialize<S: serde::Serializer>(v: &u8, s: S) -> Result<S::Ok, S::Error> {
//! #         serde::Serialize::serialize(v, s)
//! #     }
//! #     pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u8, D::Error> {
//! #         serde::Deserialize::deserialize(d)
//! #     }
//! # }
//! #[derive(Serialize, Deserialize)]
//! struct S {
//!     #[serde(frobnicate)]
//!     a: u8,
//! }
//! # fn main() {}
//! ```
//!
//! `::rename`, an unknown key that only looks like one in the set, after its
//! own accepted twin:
//!
//! ```
//! # extern crate aterm_serde as serde;
//! # use aterm_serde_derive::{Deserialize, Serialize};
//! #[derive(Serialize, Deserialize)]
//! struct S {
//!     #[serde(rename = "b")]
//!     a: u8,
//! }
//! # fn main() {}
//! ```
//!
//! ```compile_fail
//! # extern crate aterm_serde as serde;
//! # use aterm_serde_derive::{Deserialize, Serialize};
//! #[derive(Serialize, Deserialize)]
//! struct S {
//!     #[serde(::rename = "b")]
//!     a: u8,
//! }
//! # fn main() {}
//! ```
//!
//! # Containers
//!
//! Accepted twin:
//!
//! ```
//! # extern crate aterm_serde as serde;
//! # use aterm_serde_derive::{Deserialize, Serialize};
//! # #[allow(dead_code)]
//! # fn make() -> S { S { a: 1 } }
//! #[derive(Serialize, Deserialize)]
//! #[serde(rename_all = "kebab-case")]
//! struct S {
//!     a: u8,
//! }
//! # fn main() {}
//! ```
//!
//! `crate` (upstream's, outside the set):
//!
//! ```compile_fail
//! # extern crate aterm_serde as serde;
//! # use aterm_serde_derive::{Deserialize, Serialize};
//! # #[allow(dead_code)]
//! # fn make() -> S { S { a: 1 } }
//! #[derive(Serialize, Deserialize)]
//! #[serde(crate = "serde")]
//! struct S {
//!     a: u8,
//! }
//! # fn main() {}
//! ```
//!
//! `rename` on a container (in the set only for fields and variants):
//!
//! ```compile_fail
//! # extern crate aterm_serde as serde;
//! # use aterm_serde_derive::{Deserialize, Serialize};
//! # #[allow(dead_code)]
//! # fn make() -> S { S { a: 1 } }
//! #[derive(Serialize, Deserialize)]
//! #[serde(rename = "T")]
//! struct S {
//!     a: u8,
//! }
//! # fn main() {}
//! ```
//!
//! `default = "..."` on a container (only the bare form is in the set):
//!
//! ```compile_fail
//! # extern crate aterm_serde as serde;
//! # use aterm_serde_derive::{Deserialize, Serialize};
//! # #[allow(dead_code)]
//! # fn make() -> S { S { a: 1 } }
//! #[derive(Serialize, Deserialize)]
//! #[serde(default = "make")]
//! struct S {
//!     a: u8,
//! }
//! # fn main() {}
//! ```
//!
//! `rename_all` with a rule outside the five aterm writes:
//!
//! ```compile_fail
//! # extern crate aterm_serde as serde;
//! # use aterm_serde_derive::{Deserialize, Serialize};
//! # #[allow(dead_code)]
//! # fn make() -> S { S { a: 1 } }
//! #[derive(Serialize, Deserialize)]
//! #[serde(rename_all = "camelCase")]
//! struct S {
//!     a: u8,
//! }
//! # fn main() {}
//! ```
//!
//! `expecting` (upstream's, outside the set):
//!
//! ```compile_fail
//! # extern crate aterm_serde as serde;
//! # use aterm_serde_derive::{Deserialize, Serialize};
//! # #[allow(dead_code)]
//! # fn make() -> S { S { a: 1 } }
//! #[derive(Serialize, Deserialize)]
//! #[serde(expecting = "an S")]
//! struct S {
//!     a: u8,
//! }
//! # fn main() {}
//! ```
//!
//! # Variants
//!
//! Accepted twin:
//!
//! ```
//! # extern crate aterm_serde as serde;
//! # use aterm_serde_derive::{Deserialize, Serialize};
//! #[derive(Serialize, Deserialize)]
//! enum E {
//!     A(u8),
//!     #[serde(rename = "c")]
//!     B { b: u8 },
//! }
//! # fn main() {}
//! ```
//!
//! `untagged` on a variant (in the set only on a container):
//!
//! ```compile_fail
//! # extern crate aterm_serde as serde;
//! # use aterm_serde_derive::{Deserialize, Serialize};
//! #[derive(Serialize, Deserialize)]
//! enum E {
//!     A(u8),
//!     #[serde(untagged)]
//!     B { b: u8 },
//! }
//! # fn main() {}
//! ```
//!
//! `alias` on a variant (in the set only on a field):
//!
//! ```compile_fail
//! # extern crate aterm_serde as serde;
//! # use aterm_serde_derive::{Deserialize, Serialize};
//! #[derive(Serialize, Deserialize)]
//! enum E {
//!     A(u8),
//!     #[serde(alias = "c")]
//!     B { b: u8 },
//! }
//! # fn main() {}
//! ```
//!
//! `skip` on a variant (in the set only on a field):
//!
//! ```compile_fail
//! # extern crate aterm_serde as serde;
//! # use aterm_serde_derive::{Deserialize, Serialize};
//! #[derive(Serialize, Deserialize)]
//! enum E {
//!     A(u8),
//!     #[serde(skip)]
//!     B { b: u8 },
//! }
//! # fn main() {}
//! ```
//!
//! `rename_all` on a variant (in the set only on a container):
//!
//! ```compile_fail
//! # extern crate aterm_serde as serde;
//! # use aterm_serde_derive::{Deserialize, Serialize};
//! #[derive(Serialize, Deserialize)]
//! enum E {
//!     A(u8),
//!     #[serde(rename_all = "snake_case")]
//!     B { b: u8 },
//! }
//! # fn main() {}
//! ```
//!
//! # Items serde never derives for
//!
//! A union (refused upstream too), beside the struct it would be:
//!
//! ```
//! # extern crate aterm_serde as serde;
//! # use aterm_serde_derive::Serialize;
//! #[derive(Serialize)]
//! struct U {
//!     a: u8,
//! }
//! # fn main() {}
//! ```
//!
//! ```compile_fail
//! # extern crate aterm_serde as serde;
//! # use aterm_serde_derive::Serialize;
//! #[derive(Serialize)]
//! union U {
//!     a: u8,
//! }
//! # fn main() {}
//! ```
