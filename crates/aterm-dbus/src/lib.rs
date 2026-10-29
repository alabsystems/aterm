// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! First-party D-Bus for aterm: a synchronous client for unix sockets.
//!
//! Every byte the decoder reads may come from any process on the bus: `tests/mutations.rs`
//! decodes seeded damage to the recorded messages and values (`tests/recorded/`), and `fuzz/`
//! (a STOCK EXCEPTION cargo-fuzz lane: cargo-fuzz runs on a stock nightly) decodes input
//! without a seed.

#![deny(unsafe_code)]

pub mod address;
pub mod auth;
#[cfg(unix)]
pub mod conn;
mod error;
pub mod message;
pub mod names;
pub mod sig;
pub mod value;
pub mod wire;

#[cfg(unix)]
pub use conn::Connection;
pub use error::{Error, Result};
pub use message::{Kind, Message};
pub use sig::Type;
pub use value::{ObjectPath, Signature, Value};
pub use wire::Endian;
