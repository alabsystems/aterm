// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! 3-mode containment for AI agent isolation: the mode → capabilities policy, and the
//! spawn-seam actuator that enforces it. [`actuator`]'s module doc states each rule; in short:
//!
//! | Mode | Trust | Enforced |
//! |------|-------|----------|
//! | **User** | Normal (default) | Nothing beyond the capability-gated spawn; the shell keeps aterm's own resource limits. |
//! | **Safety** | Reduced | Resource limits, no OS sandbox: soft on POSIX (the shell's rc or its program can raise them), binding on Windows (the Job Object). |
//! | **Containment** | Hostile | macOS Seatbelt: no network, no task port but a process's own, no mach service lookup but the user and group queries, writes confined to the temp roots, no access to the credential and private-data stores; plus Safety's limits as hard limits. Refuses to start where no OS sandbox exists. |
//!
//! The policy tests pin the mapping over all three modes and every ordered pair: a lower mode
//! never holds more of any capability than a higher one, and Containment holds each at its
//! floor. The mode is set once per process ([`init_mode`]) and never changes.
//!
//! ## Usage
//!
//! ```rust
//! use aterm_containment::{ContainmentMode, ContainmentPolicy, init_mode};
//!
//! // At startup (called once by launcher):
//! init_mode(ContainmentMode::User).expect("mode already set");
//!
//! // Anywhere in aterm:
//! let mode = aterm_containment::mode_or_containment();
//! let caps = ContainmentPolicy::capabilities(mode);
//! ```

#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![cfg_attr(test, allow(clippy::all, clippy::pedantic))]
#![allow(
    clippy::module_name_repetitions,
    reason = "containment::ContainmentMode is clear"
)]
#![allow(
    clippy::missing_errors_doc,
    reason = "error semantics are clear from return types"
)]
#![allow(
    clippy::inline_always,
    reason = "containment hot paths require cross-crate inlining for zero-cost in User mode (#5559)"
)]

pub mod actuator;
pub(crate) mod capability;
pub mod consent;
pub(crate) mod mode;
pub(crate) mod policy;
pub mod sbpl;

// `audit` is declared last: the Trust lane (`ship trust run`) verifies roughly in
// declaration order and stops at the first fail-closed error, and `audit::forward` must
// carry the unsafe `fmt::Arguments::new` (`aterm_log::__log` takes a `fmt::Arguments`).
pub(crate) mod audit;

pub use actuator::{
    NO_OS_SANDBOX_REASON, SpawnDecision, applied_posture, decide as decide_spawn,
    network_sandbox_actuated, os_sandbox_actuated,
};
pub use audit::{log_denial, log_posture};
pub use capability::{FsCapability, NetworkCapability, ProcessCapability};
// The consent tier's types and OS entry points. Its pure helpers and `tccutil_presence`
// stay under `consent::`, so generic names do not read as the crate's own.
pub use consent::{
    Attribution, ConsentKey, ConsentPosture, DevToolGrant, DevToolList, DevToolListLabel,
    DevToolListing, DevToolProbe, DevToolProbeLabel, DevToolState, DrClass, FdaProbe, FdaScope,
    FdaState, Folder, FsConsent, ImageAnchor, PostureInputs, ProbeGate, ProbeLabel, ProbeOutcome,
    ResetAttempt, ResetOffer, ResetOfferInputs, ResetOutcome, ResetPlan, ResetStatus, Responsible,
    ResponsibleApp, ResponsibleError, SpikeEvidence, TccutilPresence, classify_dr,
    classify_image_anchor, devtool_grant, image_anchor, probe_devtool, probe_devtool_list,
    probe_fda, protected_roots, responsible_app, responsible_pid,
};
pub use mode::{ContainmentMode, ParseModeError};
pub use policy::{Capabilities, ContainmentPolicy};
pub use sbpl::{BASE_PROFILE, SANDBOX_EXEC_PATH};

use std::sync::OnceLock;

/// Global containment mode, set once at startup.
static MODE: OnceLock<ContainmentMode> = OnceLock::new();

/// Error returned when mode initialization fails.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum InitError {
    /// Mode was already initialized (cannot be changed).
    AlreadyInitialized {
        /// The mode that was previously set.
        existing: ContainmentMode,
        /// The mode that was attempted.
        attempted: ContainmentMode,
    },
}

// Hand-written `Display`/`Error`: a derived or `write!` `fmt` expands a runtime-argument
// `format_args!`, whose unsafe `fmt::Arguments::new` the Trust lane fails closed on.
impl std::fmt::Display for InitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyInitialized {
                existing,
                attempted,
            } => {
                f.write_str("containment mode already set to ")?;
                f.write_str(existing.name())?;
                f.write_str(", cannot change to ")?;
                f.write_str(attempted.name())
            }
        }
    }
}

impl std::error::Error for InitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
    }
}

/// Initialize the containment mode for this process, once at startup, before any subsystem
/// queries it. The mode never changes afterwards.
///
/// # Errors
///
/// `InitError::AlreadyInitialized` if the mode is already set.
///
/// # Panics
///
/// Unreachable: only if `OnceLock::set` fails while `OnceLock::get` returns `None`.
pub fn init_mode(mode: ContainmentMode) -> Result<(), InitError> {
    MODE.set(mode).map_err(|_| {
        let existing = *MODE.get().expect("set failed but value exists");
        InitError::AlreadyInitialized {
            existing,
            attempted: mode,
        }
    })
}

/// The current containment mode, or `None` before [`init_mode`].
#[inline(always)]
#[must_use]
pub fn try_current_mode() -> Option<ContainmentMode> {
    MODE.get().copied()
}

/// Get the current containment mode, defaulting to
/// [`ContainmentMode::Containment`] (fail-closed, the most restrictive) if
/// [`init_mode`] was never called.
#[inline(always)]
#[must_use]
pub fn mode_or_containment() -> ContainmentMode {
    try_current_mode().unwrap_or(ContainmentMode::Containment)
}

/// Initialize the mode from the launcher's `--containment` flag (`None`: no flag, so
/// `default`) and return it. No environment variable selects the mode.
///
/// # Errors
///
/// The value is not a mode (the caller fails closed), or the mode was already set.
pub fn init_mode_from_flag(
    flag: Option<&str>,
    default: ContainmentMode,
) -> Result<ContainmentMode, InitModeFromFlagError> {
    let mode = match flag {
        Some(value) => value
            .parse::<ContainmentMode>()
            .map_err(InitModeFromFlagError::Parse)?,
        None => default,
    };
    init_mode(mode).map_err(InitModeFromFlagError::Init)?;
    Ok(mode)
}

/// Error from [`init_mode_from_flag`].
#[derive(Debug)]
#[non_exhaustive]
pub enum InitModeFromFlagError {
    /// The flag's value is not a containment mode.
    Parse(ParseModeError),
    /// Mode already initialized.
    Init(InitError),
}

// Hand-written for the reason given at `InitError`'s `Display`.
impl std::fmt::Display for InitModeFromFlagError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(e) => {
                f.write_str("invalid containment mode: ")?;
                std::fmt::Display::fmt(e, f)
            }
            Self::Init(e) => std::fmt::Display::fmt(e, f),
        }
    }
}

impl std::error::Error for InitModeFromFlagError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Parse(e) => Some(e),
            Self::Init(e) => Some(e),
        }
    }
}

impl From<ParseModeError> for InitModeFromFlagError {
    fn from(value: ParseModeError) -> Self {
        Self::Parse(value)
    }
}

impl From<InitError> for InitModeFromFlagError {
    fn from(value: InitError) -> Self {
        Self::Init(value)
    }
}
