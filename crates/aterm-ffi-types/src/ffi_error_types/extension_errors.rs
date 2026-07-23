// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Extension feature FFI error types: perception, selection, bidi, IME,
//! sixel graphics, response queue, and approval workflow.

// =============================================================================
// Perception
// =============================================================================

/// Structured error codes for perception FFI v2 APIs.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum AtermPerceptionError {
    /// Operation succeeded.
    Ok = 0,

    // Null pointer errors (1-9)
    /// Null terminal pointer passed.
    ErrNullTerminal = 1,
    /// Null perception pointer passed.
    ErrNullPerception = 2,
    /// Null output buffer pointer passed.
    ErrNullBuffer = 3,
    /// Null output pointer passed.
    ErrNullOutput = 4,

    // Parameter errors (10-19)
    /// Index out of bounds.
    ErrIndexOutOfBounds = 10,
    /// Invalid region ID.
    ErrInvalidRegionId = 11,
    /// Invalid row/column point.
    ErrInvalidPoint = 12,

    // Resource errors (20-29)
    /// Allocation failed.
    ErrAllocationFailed = 20,

    // Domain-specific errors (30+)
    /// No URL found at the requested point.
    ErrNoUrlAtPoint = 30,
    /// No exit code available.
    ErrNoExitCode = 31,
    /// No shell integration block at the requested row.
    ErrNoBlockAtRow = 32,
    /// No current (in-progress) block available.
    ErrNoCurrentBlock = 33,
    /// Internal error (panic safety).
    ErrInternal = 40,

    /// Unknown error.
    ErrUnknown = 255,
}

// =============================================================================
// Selection
// =============================================================================

/// Structured error codes for selection FFI v2 APIs.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum AtermSelectionError {
    /// Operation succeeded.
    Ok = 0,

    // Null pointer errors (1-9)
    /// Null terminal pointer passed.
    ErrNullTerminal = 1,
    /// Null smart selection pointer passed.
    ErrNullSelection = 2,
    /// Null output pointer passed.
    ErrNullOutput = 3,
    /// Null rule name pointer passed.
    ErrNullName = 4,

    // Parameter errors (10-19)
    /// Row/column is out of bounds.
    ErrOutOfBounds = 10,
    /// Invalid UTF-8 in input string.
    ErrInvalidUtf8 = 11,
    /// Invalid parameter value.
    ErrInvalidParameter = 12,

    // Resource errors (20-29)
    /// Allocation failed.
    ErrAllocationFailed = 20,

    // Domain-specific errors (30+)
    /// No match found at the requested position.
    ErrNoMatch = 30,
    /// Named rule was not found.
    ErrRuleNotFound = 31,
    /// Internal error (panic safety).
    ErrInternal = 40,

    /// Unknown error.
    ErrUnknown = 255,
}

// =============================================================================
// BiDi
// =============================================================================

/// Error codes for BiDi FFI functions.
///
/// Error ranges:
/// - 0: Success
/// - 1-9: Null pointer errors
/// - 10-19: Parameter errors
/// - 30+: Domain-specific errors
/// - 255: Unknown/future
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum AtermBidiError {
    /// Success.
    Ok = 0,
    /// Resolution pointer is null.
    ErrNullResolution = 1,
    /// Output pointer is null.
    ErrNullOutput = 2,
    /// Terminal pointer is null.
    ErrNullTerminal = 3,
    /// Index out of bounds.
    ErrOutOfBounds = 10,
    /// Internal error.
    ErrInternal = 40,
    /// Unknown error (future compatibility).
    ErrUnknown = 255,
}

// =============================================================================
// IME
// =============================================================================

/// Error codes for IME FFI operations.
///
/// Following FFI_GUIDELINES.md error code ranges:
/// - 0: Success
/// - 1-9: Null pointer errors
/// - 10-19: Configuration/parameter errors
/// - 20-29: Resource errors
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum AtermImeError {
    /// Operation succeeded.
    Ok = 0,

    // Null pointer errors (1-9)
    /// Null terminal pointer passed.
    ErrNullTerminal = 1,
    /// Null text pointer passed.
    ErrNullText = 2,
    /// Null buffer pointer passed.
    ErrNullBuffer = 3,
    /// Null output parameter pointer.
    ErrNullOutput = 4,

    // Configuration/parameter errors (10-19)
    /// Invalid UTF-8 in text.
    ErrInvalidUtf8 = 10,
    /// Empty text provided.
    ErrEmptyText = 11,
    /// Zero buffer size provided.
    ErrZeroBufferSize = 12,

    // Resource errors (20-29)
    /// Buffer too small for output.
    ErrBufferTooSmall = 20,

    // Internal errors (30+)
    /// Internal error (unexpected state or panic).
    ErrInternal = 30,
}

// =============================================================================
// Sixel
// =============================================================================

/// Error codes for Sixel FFI operations.
///
/// Following FFI_GUIDELINES.md error code ranges:
/// - 0: Success
/// - 1-9: Null pointer errors
/// - 10-19: Configuration/parameter errors
/// - 20-29: Resource errors
/// - 30+: Domain-specific errors
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum AtermSixelError {
    /// Operation succeeded.
    Ok = 0,

    // Null pointer errors (1-9)
    /// Null terminal pointer passed.
    ErrNullTerminal = 1,
    /// Null output buffer pointer passed.
    ErrNullBuffer = 2,
    /// Null output pointer passed.
    ErrNullOutput = 3,

    // Domain-specific errors (30+)
    /// No Sixel image is pending.
    ErrNoImage = 30,
    /// Image dimensions overflow.
    ErrDimensionOverflow = 31,
    /// Image data is empty or invalid.
    ErrInvalidImageData = 32,
    /// Memory allocation failed.
    ErrAllocationFailed = 33,
    /// Internal error (panic caught at FFI boundary).
    ErrInternal = 99,
}

// =============================================================================
// Response
// =============================================================================

/// Structured error codes for response-draining FFI operations.
///
/// Following FFI_GUIDELINES.md error code ranges:
/// - 0: Success
/// - 1-9: Null pointer errors
/// - 10-19: Parameter errors
/// - 20-29: Resource errors
/// - 30+: Internal errors
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum AtermResponseError {
    /// Operation succeeded.
    Ok = 0,

    // Null pointer errors (1-9)
    /// Null terminal pointer passed.
    ErrNullTerminal = 1,
    /// Null buffer pointer passed.
    ErrNullBuffer = 2,
    /// Null output pointer passed.
    ErrNullOutput = 3,

    // Parameter errors (10-19)
    /// Zero buffer size provided.
    ErrZeroBufferSize = 10,
    /// Invalid parameter (e.g., size overflow).
    ErrInvalidParameter = 11,

    // Resource errors (20-29)
    /// Output buffer too small.
    ErrBufferTooSmall = 20,

    // Internal errors (30+)
    /// Internal error (panic or unexpected state).
    ErrInternal = 30,
}

// =============================================================================
// Approval (RESERVED — MCP system removed)
// =============================================================================

/// Reserved error codes for the removed MCP approval system.
///
/// This enum is kept for ABI stability — discriminant values must not be reused.
/// All approval FFI functions have been removed; this type exists only so that
/// existing binaries linked against older headers do not misinterpret error codes.
#[deprecated(note = "MCP approval system removed — kept for ABI stability only")]
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum AtermApprovalError {
    /// Operation succeeded.
    Ok = 0,

    // Null pointer errors (1-9)
    /// Null terminal pointer passed.
    ErrNullTerminal = 1,
    /// Null output pointer passed.
    ErrNullOutput = 2,

    // Parameter/state errors (10-19)
    /// Request not found for the given ID.
    ErrRequestNotFound = 10,
    /// Request is not in pending state.
    ErrNotPending = 11,
    /// Agent ID does not match the request owner.
    ErrAgentMismatch = 12,
    /// Invalid parameter (e.g., negative request ID, zero timeout).
    ErrInvalidParameter = 13,

    // Resource/feature errors (20-29)
    /// MCP feature not compiled in.
    ErrFeatureDisabled = 20,
    /// Maximum total approval requests reached.
    ErrMaxRequestsReached = 21,
    /// Maximum per-agent approval requests reached.
    ErrMaxPerAgentReached = 22,

    // Internal errors (30+)
    /// Internal error (panic or unexpected state).
    ErrInternal = 30,
}
