// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

#![deny(unsafe_op_in_unsafe_fn)]

//! 4-mode containment for AI agent isolation: a policy data model plus the
//! spawn-seam actuator that enforces it.
//!
//! It maps a [`ContainmentMode`] to a [`Capabilities`] set, records the chosen
//! mode at the spawn seam, and for `Containment` actuates an OS sandbox (macOS
//! Seatbelt via `sandbox-exec`) or refuses to start. What each mode enforces is
//! stated in [`actuator`]'s module doc; in short:
//!
//! | Mode | Trust | Enforced |
//! |------|-------|----------|
//! | **Master** | Full | Nothing beyond the capability-gated spawn. |
//! | **User** | Normal (default) | Nothing beyond the capability-gated spawn; the shell keeps aterm's own resource limits. |
//! | **Safety** | Reduced | Hardened resource limits; no OS sandbox. |
//! | **Containment** | Hostile | macOS Seatbelt: no network, no task port but a process's own (no debugger attach), writes confined to the temp roots, no read or write of the credential and private-data stores — plus the hardened limits. Refuses to start where no OS sandbox exists. |
//!
//! Core axiom: all data is code. Any byte crossing a trust boundary is a
//! potential instruction to an AI agent, so external data is untrusted by
//! default.
//!
//! ## Properties of the policy mapping
//!
//! Pinned by the unit tests in `policy.rs` and `mode.rs` and re-checked by the
//! opt-in Kani harnesses in `kani_proofs` (`scripts/verify-kani-proofs.sh`,
//! trust-mc). They concern the data model, not OS enforcement:
//!
//! - **`NonEscalation`**: mode never increases in capability
//! - **`CapabilitiesMatchMode`**: capabilities always consistent with mode
//! - **`ModeImmutableAfterInit`**: from `OnceLock` plus `NonEscalation`
//! - **`ContainmentMinimal`**: Containment is the minimal policy (every
//!   capability at its floor)
//! - **`MonotonicCapabilities`**: capabilities only decrease over time
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

#![deny(missing_docs)]
#![deny(clippy::all)]
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
    reason = "containment hot paths require cross-crate inlining for zero-cost in Master/User modes (#5559)"
)]

pub mod actuator;
pub(crate) mod capability;
pub mod consent;
#[cfg(kani)]
mod kani_proofs;
pub(crate) mod mode;
pub(crate) mod policy;
pub mod sbpl;

// `audit` is declared last: the Trust strict gate verifies roughly in
// declaration order and stops the crate sweep at the first fail-closed error.
// `audit::forward::{closure#0}` must carry the unsafe `fmt::Arguments::new`
// (`aterm_log::__log` takes a `fmt::Arguments`), so placing it last lets every
// other module be verified first.
pub(crate) mod audit;

pub use actuator::{
    NO_OS_SANDBOX_REASON, SpawnDecision, applied_posture, decide as decide_spawn,
    network_sandbox_actuated, os_sandbox_actuated,
};
pub use audit::{log_denial, log_posture};
pub use capability::{FsCapability, NetworkCapability, ProcessCapability};
// The consent tier's types and its FDA, Developer Tools and responsibility OS
// entry points. Pure helpers (`classify_probe`, `display_name`,
// `tccutil_reset_command`, `reset_offer`, …) and `tccutil_presence` stay under
// `aterm_containment::consent::` rather than the crate root, so generic names
// do not read as the crate's own and a destructive repair's entry points are
// found by reading the consent module.
pub use consent::{
    Attribution, ConsentKey, ConsentPosture, DevToolProbe, DevToolProbeLabel, DevToolState,
    DrClass, FdaProbe, FdaScope, FdaState, Folder, FsConsent, ImageAnchor, PostureInputs,
    ProbeGate, ProbeLabel, ProbeOutcome, ResetAttempt, ResetOffer, ResetOfferInputs, ResetOutcome,
    ResetPlan, ResetStatus, Responsible, ResponsibleApp, ResponsibleError, SpikeEvidence,
    TccutilPresence, classify_dr, classify_image_anchor, image_anchor, probe_devtool, probe_fda,
    protected_roots, responsible_app, responsible_pid,
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

// Hand-written `Display`/`Error`: a derived `fmt` would expand a runtime-argument
// `format_args!`, whose unsafe `fmt::Arguments::new` constructor the Trust
// strict gate's native lowering fails closed on. `ContainmentMode::name()` IS
// the mode's `Display` rendering.
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

/// Initialize the containment mode for this process.
///
/// Must be called exactly once at startup, before any subsystem queries the
/// mode. The mode is immutable afterwards (`OnceLock` single-init).
///
/// # Errors
///
/// Returns `InitError::AlreadyInitialized` if called more than once.
///
/// # Panics
///
/// Unreachable: only if `OnceLock::set` fails while `OnceLock::get` returns
/// `None`.
pub fn init_mode(mode: ContainmentMode) -> Result<(), InitError> {
    MODE.set(mode).map_err(|_| {
        let existing = *MODE.get().expect("set failed but value exists");
        InitError::AlreadyInitialized {
            existing,
            attempted: mode,
        }
    })
}

/// Get the current containment mode, if initialized.
///
/// Returns `None` if [`init_mode`] has not been called yet.
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

/// Initialize the mode from the launcher's `--containment` flag (`None` = no
/// flag: `default`). No environment variable selects the mode; the value is
/// parsed by the shared [`ContainmentMode::from_str`](std::str::FromStr).
/// Returns the resolved mode.
///
/// # Errors
///
/// Returns error if the value is not a mode (the caller fails closed) or the
/// mode was already initialized.
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

// Hand-written `Display`/`Error`/`From`, delegating to the inner errors'
// `Display`: no runtime-argument `format_args!`, whose unsafe
// `fmt::Arguments::new` constructor the Trust strict gate fails closed on.
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

#[cfg(test)]
mod tests {
    use super::*;

    // `OnceLock` is process-global, so init isolation is not testable within
    // one binary; the pure policy tests carry the verification.

    /// Verify InitError::AlreadyInitialized error message includes both modes.
    #[test]
    fn test_init_error_message_includes_modes() {
        let err = InitError::AlreadyInitialized {
            existing: ContainmentMode::Containment,
            attempted: ContainmentMode::Master,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("Containment"),
            "error should mention existing mode: {msg}"
        );
        assert!(
            msg.contains("Master"),
            "error should mention attempted mode: {msg}"
        );
    }

    /// Verify InitModeFromFlagError variants have useful messages.
    #[test]
    fn test_init_mode_from_flag_error_variants() {
        // Parse error wraps ParseModeError
        let parse_err = InitModeFromFlagError::Parse(ParseModeError("bogus".to_string()));
        let msg = parse_err.to_string();
        assert!(
            msg.contains("invalid containment mode"),
            "parse error should say what was invalid: {msg}"
        );
        assert!(
            msg.contains("bogus"),
            "parse error should include bad value: {msg}"
        );

        // Init error wraps InitError
        let init_err = InitModeFromFlagError::Init(InitError::AlreadyInitialized {
            existing: ContainmentMode::User,
            attempted: ContainmentMode::Safety,
        });
        let msg = init_err.to_string();
        assert!(
            msg.contains("already set"),
            "init error should explain double-init: {msg}"
        );
    }
}
