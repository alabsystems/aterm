// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Three-mode containment policy (`User`, `Safety`, `Containment`) and spawn-seam actuator.

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

pub(crate) mod audit;

pub use actuator::{
    NO_OS_SANDBOX_REASON, SpawnDecision, applied_posture, decide as decide_spawn,
    network_sandbox_actuated, os_sandbox_actuated,
};
pub use audit::{log_denial, log_posture};
pub use capability::{FsCapability, NetworkCapability, ProcessCapability};
pub use consent::{
    Attribution, ConsentPosture, DevToolGrant, DevToolList, DevToolListLabel, DevToolListing,
    DevToolProbe, DevToolProbeLabel, DevToolState, DrClass, FdaProbe, FdaScope, FdaState, Folder,
    FsConsent, ImageAnchor, PostureInputs, ProbeGate, ProbeLabel, ProbeOutcome, ResetAttempt,
    ResetOffer, ResetOfferInputs, ResetOutcome, ResetPlan, ResetStatus, Responsible,
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

/// Initializes the containment mode for this process once at startup.
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

/// Returns the current containment mode, defaulting to [`ContainmentMode::Containment`].
#[inline(always)]
#[must_use]
pub fn mode_or_containment() -> ContainmentMode {
    try_current_mode().unwrap_or(ContainmentMode::Containment)
}

/// Initializes the containment mode from `--containment` (or `default` when `None`).
///
/// # Errors
///
/// Returns an error if `flag` is not a valid mode or the mode was already initialized.
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
