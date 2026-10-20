// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0 AND (MIT OR Apache-2.0)
//
// A modified, std-only subset of upstream `serde_core` and `serde` 1.0.228
// (https://github.com/serde-rs/serde), dual-licensed MIT OR Apache-2.0; see
// LICENSE-MIT and LICENSE-APACHE. The `lib` facade and `tri!` below are derived
// from serde_core 1.0.228 src/crate_root.rs; the rest of this file is aterm's.

//! aterm's serde: the [`Serialize`]/[`Deserialize`] and
//! [`Serializer`]/[`Deserializer`] traits, the `ser` and `de` modules, the std
//! impls aterm's models use, and the support surface `#[derive]` output calls.
//!
//! It is upstream serde's API, carved down to what aterm compiles against
//! (docs/AUDIT-third-party-2026-10-15.md, SP.4): the same traits with the same
//! default methods, and the same error text, so a model and a format written
//! against registry serde work unchanged. Gone are the `no_std`/`alloc`
//! configurations, the `rc` and `unstable` features, and the std types no
//! aterm model carries.
//!
//! Two additions are aterm's own, and a format written against registry serde
//! needs neither. [`de::value::TextKeyDeserializer`] reads a map key a
//! string-keyed format wrote (`"7"`, `"true"`) back as the integer or `bool` it
//! was, and serde's buffered maps (`flatten`, `untagged`, `tag`) read their
//! keys through it too. [`ser::SerializeMap::serialize_field_entry`] is where a
//! struct's field reaches a map, which a format that leaves a `None` field out
//! but refuses a `None` map value (TOML) needs to tell apart.
//!
//! Dependents name it `serde` (the workspace row's `package = "aterm-serde"`).

////////////////////////////////////////////////////////////////////////////////

/// A facade around all the types we need from `std`. This avoids elaborate
/// import wrangling having to happen in every module.
mod lib {
    pub use std::{cmp, mem, ptr, slice, str};
    pub use std::{f32, f64};

    pub use std::borrow::{Cow, ToOwned};
    pub use std::boxed::Box;
    pub use std::clone;
    pub use std::collections::{BTreeMap, HashMap};
    pub use std::convert;
    pub use std::default;
    pub use std::error;
    pub use std::fmt::{self, Debug, Display, Write as FmtWrite};
    pub use std::hash::{BuildHasher, Hash};
    pub use std::io::Write;
    pub use std::marker::{self, PhantomData};
    pub use std::option;
    pub use std::path::{Path, PathBuf};
    pub use std::result;
    pub use std::string::{String, ToString};
    pub use std::vec::Vec;
}

// None of this crate's error handling needs the `From::from` error conversion
// performed implicitly by the `?` operator. Upstream measured this simplified
// macro at 5.5% less compile time than `try!` and 9% less than `?`.
macro_rules! tri {
    ($expr:expr) => {
        match $expr {
            Ok(val) => val,
            Err(err) => return Err(err),
        }
    };
}

#[macro_use]
mod macros;

pub mod de;
pub mod ser;

mod format;

#[doc(inline)]
pub use crate::de::{Deserialize, Deserializer};
#[doc(inline)]
pub use crate::ser::{Serialize, Serializer};

// Used by generated code. Not public API.
#[doc(hidden)]
mod private;

// What `#[derive]` output (crates/aterm-serde-derive) and
// `forward_to_deserialize_any!` name. Not public API. Upstream versions this
// module by serde_derive's patch number (`__private228`) so a mismatched
// derive fails to compile; aterm-serde and its derive are one workspace's
// members at one version, so the name stays plain.
#[doc(hidden)]
pub mod __private {
    #[doc(hidden)]
    pub use crate::private::*;
}

/// Derive macros, available with `features = ["derive"]`.
#[cfg(feature = "derive")]
pub use aterm_serde_derive::{Deserialize, Serialize};
