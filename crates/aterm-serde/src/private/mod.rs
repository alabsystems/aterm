// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde 1.0.228 src/private/mod.rs and serde_core 1.0.228
// src/private/mod.rs, merged and modified by the aterm project in 2026.
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

//! What `#[derive(Serialize, Deserialize)]` output calls. Not public API.

mod content;
mod seed;

pub mod de;
pub mod ser;
pub mod size_hint;
pub mod string;

pub use self::content::Content;
pub use self::seed::InPlaceSeed;

pub use crate::lib::clone::Clone;
pub use crate::lib::convert::{From, Into, TryFrom};
pub use crate::lib::default::Default;
pub use crate::lib::fmt::{self, Formatter};
pub use crate::lib::marker::PhantomData;
pub use crate::lib::option::Option::{self, None, Some};
pub use crate::lib::ptr;
pub use crate::lib::result::Result::{self, Err, Ok};

pub use self::string::from_utf8_lossy;

pub use crate::lib::{ToString, Vec};
