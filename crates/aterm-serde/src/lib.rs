// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0 AND (MIT OR Apache-2.0)
//
// A modified, std-only subset of upstream `serde_core` and `serde` 1.0.228
// (https://github.com/serde-rs/serde), dual-licensed MIT OR Apache-2.0; see
// LICENSE-MIT and LICENSE-APACHE. The `lib` facade and `tri!` below are derived
// from serde_core 1.0.228 src/crate_root.rs; the rest of this file is aterm's.

//! `serde` traits (`Serialize`, `Deserialize`, `Serializer`, `Deserializer`) and `#[derive]`
//! support.

mod lib {
    pub use std::{f32, f64};
    pub use std::{mem, ptr, slice, str};

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

#[doc(hidden)]
mod private;

#[doc(hidden)]
pub mod __private {
    #[doc(hidden)]
    pub use crate::private::*;
}

/// Derive macros, available with `features = ["derive"]`.
#[cfg(feature = "derive")]
pub use aterm_serde_derive::{Deserialize, Serialize};
