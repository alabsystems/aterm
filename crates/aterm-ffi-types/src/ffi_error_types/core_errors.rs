// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Core infrastructure FFI error types: app lifecycle, terminal operations,
//! checkpoint save/restore, configuration, detection, and memory management.

// =============================================================================
// App (runtime lifecycle)
// =============================================================================

/// Structured error codes for app runtime FFI v2 APIs.
///
/// These codes are returned from `aterm_app_*_v2` functions instead of void/sentinel.
/// The app runtime API controls app creation, tick, resize, process, and query.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum AtermAppError {
    /// Operation succeeded.
    Ok = 0,

    // Null pointer errors (1-9)
    /// Null app pointer passed.
    ErrNullApp = 1,
    /// Null data pointer passed.
    ErrNullData = 2,
    /// Null output parameter pointer passed.
    ErrNullOutput = 3,

    // Parameter/state errors (10-19)
    /// Invalid parameter value (e.g., len exceeds MAX_FFI_INPUT_BYTES).
    ErrInvalidParameter = 10,

    // Thread safety errors (20-29)
    /// Called from wrong thread (must call from creator thread).
    ErrWrongThread = 20,

    // Internal errors (30+)
    /// Internal error (unexpected state or panic).
    ErrInternal = 30,

    // Lifecycle errors (50-59)
    /// Double-free detected (debug builds only).
    ErrDoubleFree = 50,
    /// Use-after-free detected (debug builds only).
    ErrUseAfterFree = 51,
}

// =============================================================================
// Terminal
// =============================================================================

/// Structured error codes for terminal FFI v2 APIs.
///
/// These codes are returned directly from v2 APIs instead of using bool/0/null.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum AtermTerminalError {
    /// Operation succeeded.
    Ok = 0,

    // Null pointer errors (1-9)
    /// Null terminal pointer passed.
    ErrNullTerminal = 1,
    /// Null buffer pointer passed.
    ErrNullBuffer = 2,
    /// Null output pointer passed.
    ErrNullOutput = 3,

    // Parameter/state errors (10-19)
    /// Invalid parameter value.
    ErrInvalidParameter = 10,
    /// Index out of bounds (row/col/line).
    ErrOutOfBounds = 11,
    /// Operation not supported in the current state (e.g. no scrollback).
    ErrInvalidState = 12,
    /// UTF-8 decode error.
    ErrUtf8 = 13,
    /// Requested item not found (e.g. no hyperlink at column).
    ErrNotFound = 14,
    /// Reentrant call from a process() callback — would deadlock.
    ErrReentrant = 15,

    // Resource errors (20-29)
    /// Output buffer too small.
    ErrBufferTooSmall = 20,

    // Capability errors (25-29)
    /// Null capability token passed.
    ErrNullCapability = 25,
    /// Capability token is invalid (revoked or exhausted).
    ErrInvalidCapability = 26,
    /// Capability token lacks required permission.
    ErrCapabilityDenied = 27,

    // Internal errors (30+)
    /// Internal error (unexpected state).
    ErrInternal = 30,

    // Lifecycle errors (50-59)
    /// Double-free detected (debug builds only).
    ErrDoubleFree = 50,
    /// Use-after-free detected (debug builds only).
    ErrUseAfterFree = 51,
}

// Grid errors consolidated into AtermTerminalError (Part of #4299).

// =============================================================================
// Checkpoint
// =============================================================================

/// Structured error codes for checkpoint FFI v2 APIs.
///
/// These codes are returned directly from v2 APIs instead of using bool/0/null.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum AtermCheckpointError {
    /// Operation succeeded.
    Ok = 0,

    // Null pointer errors (1-9)
    /// Null checkpoint pointer passed.
    ErrNullCheckpoint = 1,
    /// Null terminal pointer passed.
    ErrNullTerminal = 2,
    /// Null output pointer passed.
    ErrNullOutput = 3,

    // I/O errors (20-29)
    /// Failed to write checkpoint.
    ErrWriteFailed = 20,
    /// Failed to read checkpoint.
    ErrReadFailed = 21,
    /// Checkpoint file is corrupted or incompatible.
    ErrCorrupted = 22,

    // Capability errors (30-39)
    /// Null capability token pointer passed.
    ErrNullCapability = 30,
    /// Capability token is revoked or exhausted.
    ErrInvalidCapability = 31,
    /// Capability token lacks required permission.
    ErrCapabilityDenied = 32,

    // Internal errors (40+)
    /// Internal error (unexpected state or panic).
    ErrInternal = 40,
}

// =============================================================================
// Config
// =============================================================================

/// Structured error codes for config FFI operations.
///
/// These codes enable programmatic error handling without parsing string messages.
/// V2 config APIs return these codes directly; the V1 `aterm_config_last_error()`
/// is a no-op stub that always returns NULL (#4802).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum AtermConfigError {
    /// Operation succeeded.
    Ok = 0,

    // Null pointer errors (1-9)
    /// Null path pointer passed.
    ErrNullPath = 1,
    /// Null config pointer passed.
    ErrNullConfig = 2,
    /// Null watcher pointer passed.
    ErrNullWatcher = 3,
    /// Null output pointer passed.
    ErrNullOutput = 4,
    /// Null buffer pointer passed.
    ErrNullBuffer = 5,

    // Parameter/encoding errors (10-19)
    /// Invalid UTF-8 in path or data.
    ErrInvalidUtf8 = 10,
    /// Configuration file not found at path.
    ErrFileNotFound = 11,
    /// TOML parse error.
    ErrParseError = 12,
    /// Path encoding error for CString conversion.
    ErrPathEncoding = 13,

    // I/O errors (20-29)
    /// Failed to read configuration file.
    ErrFileRead = 20,
    /// Failed to write configuration file.
    ErrFileWrite = 21,
    /// Output buffer too small.
    ErrBufferTooSmall = 22,

    // Watcher errors (30-39)
    /// Failed to create configuration watcher.
    ErrWatcherCreate = 30,

    // Internal errors (40+)
    /// Internal error (unexpected state or panic).
    ErrInternal = 40,
}

// =============================================================================
// Detection
// =============================================================================

/// Structured error codes for detection FFI v2 APIs.
///
/// These codes are returned directly from v2 APIs instead of using bool/0/null.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum AtermDetectionError {
    /// Operation succeeded.
    Ok = 0,

    // Null pointer errors (1-9)
    /// Null terminal pointer passed.
    ErrNullTerminal = 1,
    /// Null output pointer passed.
    ErrNullOutput = 2,

    // Internal errors (10-19)
    /// Internal error (panic caught at FFI boundary).
    ErrInternal = 10,
}

// =============================================================================
// Memory
// =============================================================================

/// Error codes for Memory FFI operations.
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
pub enum AtermMemoryError {
    /// Operation succeeded.
    Ok = 0,

    // Null pointer errors (1-9)
    /// Null path pointer passed.
    ErrNullPath = 1,
    /// Null store pointer passed.
    ErrNullStore = 2,
    /// Null session ID pointer passed.
    ErrNullSessionId = 3,
    /// Null output pointer passed.
    ErrNullOutput = 4,
    /// Null memory handle pointer passed.
    ErrNullMemory = 5,
    /// Null command string pointer passed.
    ErrNullCommand = 6,

    // Configuration errors (10-19)
    /// Invalid UTF-8 in input string.
    ErrInvalidUtf8 = 10,
    /// Invalid path (not a valid file path).
    ErrInvalidPath = 11,

    // Resource errors (20-29)
    /// IO error (failed to open/read/write).
    ErrIoError = 20,
    /// Database error.
    ErrDatabaseError = 21,

    // Domain-specific errors (30+)
    /// Memory feature is not compiled in.
    ErrFeatureDisabled = 30,
    /// Session not found.
    ErrSessionNotFound = 31,
    /// Command not found.
    ErrCommandNotFound = 32,
    /// File not found.
    ErrFileNotFound = 33,

    // Internal errors (40+)
    /// Internal error (unexpected state or panic).
    ErrInternal = 40,

    // Thread safety errors (50-59)
    /// Called from wrong thread (must call from creator thread).
    ErrWrongThread = 50,
}
