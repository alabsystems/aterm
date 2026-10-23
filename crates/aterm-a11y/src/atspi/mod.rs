// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! AT-SPI publisher serving a window's accessibility tree on Linux screen-reader buses.

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
pub use publisher::{Buses, Publisher, SessionBuses, WRITE};

#[cfg(test)]
mod oracle;
#[cfg(test)]
mod tests;
