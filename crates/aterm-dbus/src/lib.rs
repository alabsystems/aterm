// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// Registers the `trust` tool so the `#[cfg_attr(trust_verify, trust::…)]`
// contracts on the decoder's arithmetic resolve. A stable build ignores this.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! First-party D-Bus for aterm: a synchronous client for unix sockets.
//!
//! It replaces zbus and zvariant, which reach Linux only through the AT-SPI
//! publisher (docs/AUDIT-third-party-2026-10-15.md, unit a11y, step A11Y.3).
//! It is written against the D-Bus specification, not carved from zbus: zbus
//! is async throughout and cannot be cut down to a blocking client (the
//! ruling's decision 6). Instead its bytes are the oracle. `tests/recorder`,
//! a crate outside the workspace, recorded registry zbus 5.16.0 / zvariant
//! 5.12.0 once (its Cargo.toml says how to again), and the tests hold this
//! crate to those bytes:
//!
//! * `tests/recorded/zvariant-5.12.0-values.tsv`: the value shapes the AT-SPI
//!   publisher sends or reads (object references, event bodies, Cache items,
//!   property maps, relation sets), each basic type at its edges, and
//!   alignment at odd offsets, encoded by zvariant in both byte orders;
//! * `tests/recorded/zbus-5.16.0-messages.tsv`: whole messages built by zbus,
//!   the bus's answers and the publisher's events among them;
//! * `tests/recorded/zbus-5.16.0-session.tsv`: what a zbus client wrote to a
//!   scripted bus, from the SASL handshake to a signal, over a unix socket and
//!   over one that passes no descriptors.
//!
//! What it speaks:
//!
//! * [`wire`]: the marshalling format for every type code but `h` (unix
//!   descriptors), in both byte orders, over [`Value`] and [`Type`];
//! * [`message`]: the header, its fields and the framing;
//! * [`auth`]: SASL EXTERNAL;
//! * [`address`]: address lists and the session bus's address;
//! * [`Connection`] (unix): `Hello`, method calls with a deadline, replies,
//!   errors, signals and `AddMatch`, all blocking, over an
//!   [`aterm_uds::CtlStream`].
//!
//! Untrusted input: every byte the decoder reads may come from any process
//! on the bus. It returns an error and never panics, accepts only canonical
//! encodings, and allocates linearly in what it reads; see [`wire`] for the
//! rules it holds and `conn::DEFAULT_MAX_INCOMING` for a connection's size
//! limit. `tests/mutations.rs` holds that over seeded damage to every
//! recording, and `fuzz/` (a STOCK EXCEPTION cargo-fuzz lane) without a seed.
//!
//! Its first consumer is the Linux desktop-notification delivery
//! (aterm-gui's `notify`: `org.freedesktop.Notifications.Notify` on the
//! session bus). The AT-SPI publisher (A11Y.4) moves onto it once the Linux
//! AT-SPI lane can check it end to end.

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
