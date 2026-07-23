// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! OSC 1337 multipart file transfer types.

/// Maximum size for multipart file transfers (16 MB).
///
/// This limit prevents memory exhaustion from malicious or runaway file transfers.
pub const MULTIPART_FILE_MAX_SIZE: usize = 16 * 1024 * 1024;

/// Operation type for OSC 1337 multipart file transfers.
///
/// The multipart file protocol allows chunked file transfers for tmux compatibility.
///
/// # Protocol
///
/// - `ESC ] 1337 ; MultipartFile=name;size=N ST` - Start file transfer
/// - `ESC ] 1337 ; FilePart=base64 ST` - Send file chunk (may repeat)
/// - `ESC ] 1337 ; FileEnd ST` - Complete transfer
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MultipartFileOperation {
    /// File transfer completed successfully.
    Complete {
        /// Original filename from the MultipartFile command
        name: String,
        /// Declared file size (may differ from actual data length)
        declared_size: usize,
        /// Assembled file data (base64-decoded, concatenated chunks)
        data: Vec<u8>,
    },
    /// File transfer failed (size exceeded, decode error, etc.).
    Failed {
        /// Filename if known
        name: Option<String>,
        /// Reason for failure
        reason: String,
    },
}

/// State for tracking an in-progress multipart file transfer.
#[derive(Debug, Clone)]
pub struct MultipartFileState {
    /// Original filename
    pub name: String,
    /// Declared file size from the MultipartFile command
    pub declared_size: usize,
    /// Accumulated file data (base64-decoded chunks)
    pub data: Vec<u8>,
}

impl MultipartFileState {
    /// Create a new multipart file state.
    pub fn new(name: String, declared_size: usize) -> Self {
        let capacity = declared_size.min(MULTIPART_FILE_MAX_SIZE);
        Self {
            name,
            declared_size,
            data: Vec::with_capacity(capacity),
        }
    }

    /// Append decoded chunk data.
    ///
    /// Returns `Err` if the total size would exceed the maximum.
    // Skip: Vec::extend_from_slice alloc class (its arithmetic is now saturating and proven).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn append(&mut self, chunk: &[u8]) -> Result<(), &'static str> {
        // saturating_add: two real slice lens cannot overflow usize; saturation is
        // exact for every real input and takes the same Err arm on the boundary.
        if self.data.len().saturating_add(chunk.len()) > MULTIPART_FILE_MAX_SIZE {
            return Err("multipart file exceeds maximum size");
        }
        self.data.extend_from_slice(chunk);
        Ok(())
    }

    /// Check if the accumulated data matches the declared size.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.data.len() >= self.declared_size
    }
}
