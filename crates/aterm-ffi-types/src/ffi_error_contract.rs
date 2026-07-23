// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Unified FFI error contract types.
//!
//! Provides a cross-crate error envelope (`AtermErrorDomain`, `AtermErrorKind`,
//! `AtermErrorInfo`) and a trait (`AtermFfiErrorCode`) so that domain-specific
//! `Aterm*Error` enums can be resolved through one generic API.
//!
//! See `designs/2026-02-14-2809-unified-ffi-error-contract.md` for motivation
//! and architecture.

/// Error domain identifier for cross-module error classification.
///
/// Each domain corresponds to a module-specific `Aterm*Error` enum.
/// Values are stable ABI and must not be reordered or removed.
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtermErrorDomain {
    /// Legacy `AtermError` (negative codes, deprecated).
    CoreLegacy = 1,
    /// `AtermTerminalError` — terminal operations.
    Terminal = 2,
    /// Grid/cell operations (consolidated into `AtermTerminalError`).
    Grid = 3,
    /// VT parser (consolidated into `AtermTerminalError`).
    Parser = 4,
    /// `AtermCheckpointError` — checkpoint save/restore.
    Checkpoint = 5,
    /// `AtermConfigError` — configuration.
    Config = 6,
    /// `AtermDetectionError` — terminal detection.
    Detection = 7,
    /// `AtermGraphicsError` — image/graphics protocol.
    Graphics = 8,
    /// `AtermSixelError` — Sixel graphics.
    Sixel = 9,
    /// `AtermImeError` — input method editor.
    Ime = 10,
    /// `AtermMemoryError` — memory/session management.
    Memory = 11,
    /// `AtermSelectionError` — text selection.
    Selection = 12,
    /// `AtermPerceptionError` — perception/accessibility.
    Perception = 13,
    /// `AtermCapabilityError` — capability tokens.
    Capability = 14,
    /// `AtermResponseError` — DSR/DA response reading.
    Response = 15,
    /// `AtermUIError` — UI bridge.
    Ui = 16,
    /// Reserved — MCP system removed. Discriminant kept for ABI stability.
    Mcp = 17,
    /// Trigger/automation (consolidated into `AtermTerminalError`).
    Trigger = 18,
    /// `AtermEditorError` — editor integration.
    Editor = 19,
    /// `AtermSearchError` — search operations.
    Search = 20,
    /// `AtermGpuError` — GPU FFI.
    Gpu = 21,
    /// `AtermRenderError` — GPU renderer.
    Render = 22,
    /// `AtermMediaError` — media playback.
    Media = 23,
    /// `AtermBidiError` — bidirectional text.
    Bidi = 24,
    /// Reserved — MCP approval system removed. Discriminant kept for ABI stability.
    Approval = 25,
}

impl AtermErrorDomain {
    /// Restore a domain from its stable `u16` discriminant.
    ///
    /// Total: tags that match no domain map to `CoreLegacy` — unreachable
    /// from `AtermFfiErrorCode::DOMAIN_TAG`, which is always a real domain's
    /// discriminant. Kept `#[inline(never)]` so its enum-valued constants
    /// stay out of the MIR of generic callers (unsupported by the Trust full
    /// verifier).
    #[inline(never)]
    pub const fn from_tag(tag: u16) -> Self {
        match tag {
            2 => Self::Terminal,
            3 => Self::Grid,
            4 => Self::Parser,
            5 => Self::Checkpoint,
            6 => Self::Config,
            7 => Self::Detection,
            8 => Self::Graphics,
            9 => Self::Sixel,
            10 => Self::Ime,
            11 => Self::Memory,
            12 => Self::Selection,
            13 => Self::Perception,
            14 => Self::Capability,
            15 => Self::Response,
            16 => Self::Ui,
            17 => Self::Mcp,
            18 => Self::Trigger,
            19 => Self::Editor,
            20 => Self::Search,
            21 => Self::Gpu,
            22 => Self::Render,
            23 => Self::Media,
            24 => Self::Bidi,
            25 => Self::Approval,
            _ => Self::CoreLegacy,
        }
    }
}

/// Error kind for generic error classification.
///
/// Maps module-specific error codes to coarse categories that hosts can
/// handle without knowing every domain enum.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtermErrorKind {
    /// No error.
    Ok = 0,
    /// A required pointer was null.
    NullPointer = 1,
    /// An invalid parameter was provided.
    Parameter = 2,
    /// A resource constraint (buffer too small, I/O failure, etc.).
    Resource = 3,
    /// Capability token error (null, revoked, denied).
    Capability = 4,
    /// Domain-specific error not covered by generic kinds.
    DomainSpecific = 5,
    /// Internal/panic error.
    Internal = 6,
    /// Unknown error code for the given domain.
    Unknown = 255,
}

/// Cross-module error envelope for generic host handling.
///
/// Hosts receive this from the resolver API and can branch on `kind` for
/// generic handling or on `(domain, code)` for specific handling.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtermErrorInfo {
    /// Which module the error came from.
    pub domain: AtermErrorDomain,
    /// The raw error code within that domain.
    pub code: u16,
    /// Coarse classification.
    pub kind: AtermErrorKind,
    /// Reserved for future use (padding to 6 bytes).
    pub reserved: u8,
}

impl AtermErrorInfo {
    /// Construct an `Ok` info for a given domain.
    #[must_use]
    pub const fn ok(domain: AtermErrorDomain) -> Self {
        Self {
            domain,
            code: 0,
            kind: AtermErrorKind::Ok,
            reserved: 0,
        }
    }

    /// Construct an error info from parts.
    #[must_use]
    pub const fn new(domain: AtermErrorDomain, code: u16, kind: AtermErrorKind) -> Self {
        Self {
            domain,
            code,
            kind,
            reserved: 0,
        }
    }

    /// Construct an unknown-code info.
    #[must_use]
    pub const fn unknown(domain: AtermErrorDomain, code: u16) -> Self {
        Self {
            domain,
            code,
            kind: AtermErrorKind::Unknown,
            reserved: 0,
        }
    }
}

/// Trait for domain-specific FFI error enums to integrate with the unified
/// error contract.
///
/// Implement this for each `Aterm*Error` enum so the resolver can map
/// `(domain, code)` pairs to `AtermErrorInfo`.
pub trait AtermFfiErrorCode: Copy {
    /// The error domain this enum belongs to.
    const DOMAIN: AtermErrorDomain;

    /// `Self::DOMAIN` as its stable `u16` discriminant. Do not override.
    ///
    /// Exists because the Trust full verifier cannot lower an unevaluated
    /// *enum*-typed associated const in a generic function's MIR ("refusing
    /// to prove with a guessed value"), while an integer const lowers
    /// cleanly; [`AtermErrorDomain::from_tag`] restores the domain value in
    /// [`Self::info`].
    #[doc(hidden)]
    const DOMAIN_TAG: u16 = Self::DOMAIN as u16;

    /// The raw numeric code for this variant.
    fn code(self) -> u16;

    /// The coarse error kind for this variant.
    fn kind(self) -> AtermErrorKind;

    /// Construct a `AtermErrorInfo` from this error.
    fn info(self) -> AtermErrorInfo {
        // Trust L0: materialize the domain via its u16 tag instead of
        // `Self::DOMAIN` directly — an unevaluated enum-typed associated
        // const is unsupported MIR for the Trust full verifier anywhere in a
        // generic function, while an integer const lowers cleanly. The
        // tag roundtrip is a proven identity (see
        // `domain_from_tag_roundtrips_for_all_variants`), so behavior is
        // unchanged.
        let tag = Self::DOMAIN_TAG;
        let domain = AtermErrorDomain::from_tag(tag);
        let code = self.code();
        let kind = self.kind();
        AtermErrorInfo::new(domain, code, kind)
    }
}

/// Helper macro for implementing `AtermFfiErrorCode` on a `#[repr(C)]` error enum.
///
/// Generates the `code()` method via `as u16` cast and a match-based `kind()`.
///
/// # Example
///
/// ```no_run
/// use aterm_ffi_types::{AtermErrorDomain, AtermErrorKind};
///
/// #[repr(C)]
/// #[derive(Copy, Clone)]
/// enum MyError { Ok = 0, ErrNull = 1, ErrInternal = 2 }
///
/// aterm_ffi_types::impl_aterm_ffi_error_code! {
///     MyError => AtermErrorDomain::Terminal => {
///         Ok => AtermErrorKind::Ok,
///         ErrNull => AtermErrorKind::NullPointer,
///         ErrInternal => AtermErrorKind::Internal,
///     }
/// }
/// ```
#[macro_export]
macro_rules! impl_aterm_ffi_error_code {
    ($enum_name:ident => $domain:expr_2021 => { $($variant:ident => $kind:expr_2021),+ $(,)? }) => {
        impl $crate::AtermFfiErrorCode for $enum_name {
            const DOMAIN: $crate::AtermErrorDomain = $domain;

            fn code(self) -> u16 {
                self as u16
            }

            fn kind(self) -> $crate::AtermErrorKind {
                match self {
                    $(Self::$variant => $kind,)+
                }
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FakeError {
        Ok = 0,
        ErrNull = 1,
        ErrBad = 10,
        ErrInternal = 30,
    }

    impl_aterm_ffi_error_code! {
        FakeError => AtermErrorDomain::Terminal => {
            Ok => AtermErrorKind::Ok,
            ErrNull => AtermErrorKind::NullPointer,
            ErrBad => AtermErrorKind::Parameter,
            ErrInternal => AtermErrorKind::Internal,
        }
    }

    #[test]
    fn trait_code_returns_discriminant() {
        assert_eq!(FakeError::Ok.code(), 0);
        assert_eq!(FakeError::ErrNull.code(), 1);
        assert_eq!(FakeError::ErrBad.code(), 10);
        assert_eq!(FakeError::ErrInternal.code(), 30);
    }

    #[test]
    fn trait_kind_maps_correctly() {
        assert_eq!(FakeError::Ok.kind(), AtermErrorKind::Ok);
        assert_eq!(FakeError::ErrNull.kind(), AtermErrorKind::NullPointer);
        assert_eq!(FakeError::ErrBad.kind(), AtermErrorKind::Parameter);
        assert_eq!(FakeError::ErrInternal.kind(), AtermErrorKind::Internal);
    }

    #[test]
    fn trait_info_assembles_envelope() {
        let info = FakeError::ErrNull.info();
        assert_eq!(info.domain, AtermErrorDomain::Terminal);
        assert_eq!(info.code, 1);
        assert_eq!(info.kind, AtermErrorKind::NullPointer);
        assert_eq!(info.reserved, 0);
    }

    #[test]
    fn error_info_ok_constructor() {
        let info = AtermErrorInfo::ok(AtermErrorDomain::Grid);
        assert_eq!(info.domain, AtermErrorDomain::Grid);
        assert_eq!(info.code, 0);
        assert_eq!(info.kind, AtermErrorKind::Ok);
    }

    #[test]
    fn error_info_unknown_constructor() {
        let info = AtermErrorInfo::unknown(AtermErrorDomain::Parser, 999);
        assert_eq!(info.domain, AtermErrorDomain::Parser);
        assert_eq!(info.code, 999);
        assert_eq!(info.kind, AtermErrorKind::Unknown);
    }

    /// `from_tag` must be the exact inverse of `as u16` for every domain —
    /// this is the identity `AtermFfiErrorCode::info` relies on when routing
    /// `Self::DOMAIN` through `DOMAIN_TAG`.
    #[test]
    fn domain_from_tag_roundtrips_for_all_variants() {
        use AtermErrorDomain as D;
        for domain in [
            D::CoreLegacy,
            D::Terminal,
            D::Grid,
            D::Parser,
            D::Checkpoint,
            D::Config,
            D::Detection,
            D::Graphics,
            D::Sixel,
            D::Ime,
            D::Memory,
            D::Selection,
            D::Perception,
            D::Capability,
            D::Response,
            D::Ui,
            D::Mcp,
            D::Trigger,
            D::Editor,
            D::Search,
            D::Gpu,
            D::Render,
            D::Media,
            D::Bidi,
            D::Approval,
        ] {
            assert_eq!(D::from_tag(domain as u16), domain);
        }
    }

    /// Tags outside the known range fall back to `CoreLegacy` (total
    /// function, no panic).
    #[test]
    fn domain_from_tag_is_total() {
        assert_eq!(AtermErrorDomain::from_tag(0), AtermErrorDomain::CoreLegacy);
        assert_eq!(
            AtermErrorDomain::from_tag(u16::MAX),
            AtermErrorDomain::CoreLegacy
        );
    }

    #[test]
    fn domain_values_are_stable() {
        assert_eq!(AtermErrorDomain::CoreLegacy as u16, 1);
        assert_eq!(AtermErrorDomain::Terminal as u16, 2);
        assert_eq!(AtermErrorDomain::Bidi as u16, 24);
        assert_eq!(AtermErrorDomain::Approval as u16, 25);
    }

    #[test]
    fn kind_values_are_stable() {
        assert_eq!(AtermErrorKind::Ok as u8, 0);
        assert_eq!(AtermErrorKind::NullPointer as u8, 1);
        assert_eq!(AtermErrorKind::Unknown as u8, 255);
    }
}
