// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Embedded replicated application database over astream. Every replica owns its
//! local strict journal; authenticated immutable operations merge after partitions.
//! One replica needs no service. Any two may exchange directly. See the database
//! design for offline revocation, rollback and compromised-writer assumptions.
#![forbid(unsafe_code)]
#![cfg_attr(all(anchor_rules, any(test, debug_assertions)), feature(macro_attr))]

use std::fmt;

pub mod accounts;
#[cfg(unix)]
pub mod cli;
pub mod contacts;
#[cfg(unix)]
pub mod control;
pub mod crypto;
pub mod database;
#[cfg(unix)]
pub mod import;
#[cfg(unix)]
pub mod local;
pub mod machines;
#[cfg(unix)]
pub mod network;

/// A refusal that identifies the failed boundary without quoting payloads or keys.
#[derive(Debug)]
pub struct Error(pub String);
impl Error {
    pub(crate) fn new(reason: &str) -> Self {
        Self(reason.into())
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self(value.to_string())
    }
}
/// Database boundary result.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(all(test, unix))]
mod tests;
