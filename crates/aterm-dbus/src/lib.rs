// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! First-party D-Bus for aterm: a synchronous client for unix sockets.

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
