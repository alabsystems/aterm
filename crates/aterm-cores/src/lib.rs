// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The cores this process's own thread pools may use.
//!
//! A pool that sizes itself to the machine fills it: a test binary whose test runs one over every
//! core uses the whole Mac from one libtest thread, whatever the verifier charged it
//! (ATERM-KI-1103-3). [`available_parallelism`] is the one place a pool's size comes from in this
//! workspace (`clippy.toml` bans `std::thread::available_parallelism` everywhere else): the
//! cores the process may run on, at most the grant [`GRANT_ENV`] names.
//!
//! `aterm-verify` sets [`GRANT_ENV`] on every test binary it runs to the cores each of its test
//! threads may use: the binary's charge divided by its libtest width, at least one. A child the
//! test starts inherits it, so an `aterm` a test boots sizes its own pools to that test's grant
//! too. With the variable unset (every process a person runs) the answer is exactly
//! `std::thread::available_parallelism`, so production pools keep the machine's width.

use std::ffi::OsStr;
use std::num::NonZeroUsize;

/// The variable that grants a process at most this many cores for its own pools: a positive
/// decimal. `aterm-verify` sets it per test binary; nothing else does.
pub const GRANT_ENV: &str = "ATERM_TEST_CORES";

/// The cores this process's pools may use: `std::thread::available_parallelism`, at most the
/// grant [`GRANT_ENV`] names ([`bounded`]).
///
/// # Errors
/// Where the machine's parallelism is unknown and no grant is set, std's error.
#[inline]
pub fn available_parallelism() -> std::io::Result<NonZeroUsize> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the one read of the machine's parallelism a pool is sized from: bounded here"
    )]
    let machine = std::thread::available_parallelism();
    bounded(machine, std::env::var_os(GRANT_ENV).as_deref())
}

/// The cores a pool every test thread of this test binary shares may use: the machine, at most
/// the per-thread grant [`GRANT_ENV`] names times the binary's libtest width (`RUST_TEST_THREADS`,
/// [`WIDTH_ENV`]), so a process-wide gate (a semaphore every sweep takes, a static worker
/// ceiling) holds the binary's whole grant rather than one thread's share of it
/// ([`bounded_for_binary`]).
///
/// Only code that runs in the libtest process itself reads this (test code, or a `cfg(test)`
/// branch): a child a test starts inherits both variables but is that one test's work, so its
/// pools take [`available_parallelism`].
///
/// # Errors
/// Where the machine's parallelism is unknown and no grant is set, std's error.
#[inline]
pub fn binary_parallelism() -> std::io::Result<NonZeroUsize> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the one read of the machine's parallelism a shared pool is sized from: bounded here"
    )]
    let machine = std::thread::available_parallelism();
    bounded_for_binary(
        machine,
        std::env::var_os(GRANT_ENV).as_deref(),
        std::env::var_os(WIDTH_ENV).as_deref(),
    )
}

/// The libtest width a test binary runs at; read only by [`binary_parallelism`].
pub const WIDTH_ENV: &str = "RUST_TEST_THREADS";

/// `machine`, at most `grant` ([`grant`]) times `width` (a positive decimal; one where it is
/// unset or anything else, so a width it cannot read never widens the grant); `machine`
/// unchanged with no grant, and the grant times the width where the machine is unknown.
///
/// # Errors
/// `machine`'s error, where no grant is set.
#[inline]
pub fn bounded_for_binary(
    machine: std::io::Result<NonZeroUsize>,
    grant: Option<&OsStr>,
    width: Option<&OsStr>,
) -> std::io::Result<NonZeroUsize> {
    let width = width
        .and_then(OsStr::to_str)
        .filter(|text| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|text| text.parse::<NonZeroUsize>().ok())
        .unwrap_or(NonZeroUsize::MIN);
    let total = self::grant(grant).map(|g| g.saturating_mul(width));
    match (machine, total) {
        (Ok(machine), Some(total)) => Ok(machine.min(total)),
        (Ok(machine), None) => Ok(machine),
        (Err(_), Some(total)) => Ok(total),
        (Err(why), None) => Err(why),
    }
}

/// `machine`, at most `grant` ([`grant`]); the grant alone where the machine is unknown.
///
/// # Errors
/// `machine`'s error, where no grant is set.
#[inline]
pub fn bounded(
    machine: std::io::Result<NonZeroUsize>,
    grant: Option<&OsStr>,
) -> std::io::Result<NonZeroUsize> {
    bounded_for_binary(machine, grant, None)
}

/// The cores a [`GRANT_ENV`] value grants: `None` where it is unset, its number where it is a
/// positive decimal, and one core where it is set to anything else (empty, zero, a word). A set
/// variable states that a grant exists, and only the verifier sets it; a value it cannot read
/// gets the narrowest pool, which costs a test speed and never its result, rather than the whole
/// machine, which costs every other binary its cores.
#[must_use]
#[inline]
pub fn grant(value: Option<&OsStr>) -> Option<NonZeroUsize> {
    let value = value?;
    Some(
        value
            .to_str()
            .filter(|text| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|text| text.parse::<NonZeroUsize>().ok())
            .unwrap_or(NonZeroUsize::MIN),
    )
}

#[cfg(test)]
mod tests;
