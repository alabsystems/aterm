// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The AT-SPI publisher: a window's AccessKit tree, served to Linux screen
//! readers on at-spi2-core's accessibility bus.
//!
//! A window makes an [`Adapter`] ([`Publisher::adapter`], with the process's
//! [`Publisher::global`]) and hands it a full tree when the publisher asks
//! for one ([`crate::model::ActivationHandler`]), then updates while an
//! assistive technology listens ([`Adapter::update_if_active`]). The
//! publisher answers the AT's calls from the tree on its own thread and
//! passes the AT's actions back through [`crate::model::ActionHandler`]: aterm
//! advertises Focus, Click, SetValue, ScrollUp, ScrollDown,
//! ReplaceSelectedText and SetTextSelection.
//!
//! What goes on the wire is what accesskit_unix 0.22 sent, which this module
//! replaces with a blocking client over aterm-dbus
//! (docs/AUDIT-third-party-2026-10-15.md, unit a11y, step A11Y.4): the same
//! object paths, interfaces, methods, signatures, events and error texts.
//! The semantics, which node answers what, are accesskit_atspi_common's,
//! carved into `semantics` (A11Y.5a) and held to its recorded answers by
//! `oracle`. The differences are its failure modes: a bus that closes is
//! logged and survived, never a panic, the publisher rejoins the
//! accessibility bus when at-spi2-core brings one back, and a blank line in
//! a document no longer hangs a paragraph search.
//!
//! * `adapter`: the window's handle and the state it shares with the thread;
//! * `publisher`: the thread, its buses and its loop;
//! * `proxies`: the calls it makes of at-spi2-core and the registry;
//! * `serve`: the object table and every method it answers;
//! * `semantics`: what each node answers, and the events an update sends;
//! * `types`: AT-SPI's interfaces, states, roles and enums, as numbers;
//! * `events`: the signals a tree change sends;
//! * `wire`: the values on the wire.

mod adapter;
mod events;
mod proxies;
mod publisher;
mod semantics;
mod serve;
mod types;
mod wire;

pub use crate::model::{ActionHandler, ActivationHandler, DeactivationHandler};
pub use adapter::Adapter;
pub use publisher::{Buses, Publisher, SessionBuses};

#[cfg(test)]
mod oracle;
#[cfg(test)]
mod tests;
