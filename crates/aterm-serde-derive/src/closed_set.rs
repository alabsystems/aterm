// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Compile-fail controls for the closed attribute set.
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
