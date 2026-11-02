// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Ordinary Git routing and installed fleet identity, supplied by the executable.

use crate::Result;
use std::path::Path;
use std::time::Instant;

/// An existing fleet identity read by the executable, never created by this probe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    /// Random 128-bit fleet identifier in its ordinary lowercase encoding.
    pub id: String,
    /// The actual installed fleet signing public key.
    pub key: [u8; 32],
}

/// One bounded ordinary-Git invocation and its fresh local authorization check.
pub struct Invocation<'a> {
    /// The authenticated repository owner.
    pub owner: &'a str,
    /// The public login proved by the selected native credential route.
    pub principal: &'a str,
    /// This probe's private scratch repository.
    pub scratch: &'a Path,
    /// Shared absolute bound for all commands in this operation.
    pub deadline: Instant,
    /// Revalidate current local membership and credentials immediately before each child.
    pub current: &'a dyn Fn() -> Result<()>,
}

/// Whether a Git mutation could have reached the remote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectError {
    /// No mutation child started; the same intent may be attempted again.
    NotStarted,
    /// A child started, so recovery must observe the exact remote object first.
    Uncertain,
}

/// A narrow composition port. Every Git call retains ordinary configuration and repeats
/// same-repository effective-route admission before a bounded network command.
pub trait Git: Send + Sync {
    /// Read the existing local identity. Missing identity must refuse, never mint one.
    fn node(&self) -> Result<Node>;
    /// Fetch only the target repository's advertised HEAD into an owned scratch repository.
    /// The returned value is that target's commit, never a local development HEAD.
    fn fetch_head(&self, invocation: &Invocation<'_>) -> Result<String>;
    /// Read exactly this remote tag with the ordinary effective route.
    fn tag(&self, invocation: &Invocation<'_>, tag: &str) -> Result<Option<String>>;
    /// Create only an absent tag, with an explicit empty expected-OID lease, then read it back.
    fn create_tag(
        &self,
        invocation: &Invocation<'_>,
        tag: &str,
        oid: &str,
    ) -> std::result::Result<(), EffectError>;
    /// Delete only this tag at the exact recorded OID, then read its absence back.
    fn delete_tag(
        &self,
        invocation: &Invocation<'_>,
        tag: &str,
        oid: &str,
    ) -> std::result::Result<(), EffectError>;
}
