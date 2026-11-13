// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`SecretBytes`]: an owned secret that is wiped when dropped and never printed.

use zeroize::Zeroize;

/// Bytes that are a secret (a value, a key, a plaintext).
#[derive(Default, PartialEq, Eq)]
pub struct SecretBytes(Vec<u8>);

impl SecretBytes {
    /// Take ownership of `bytes`.
    #[inline]
    #[must_use]
    pub const fn new(bytes: Vec<u8>) -> Self {
        SecretBytes(bytes)
    }

    /// The bytes.
    #[inline]
    #[must_use]
    pub const fn as_slice(&self) -> &[u8] {
        self.0.as_slice()
    }

    /// The bytes as UTF-8, if they are.
    #[inline]
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.0).ok()
    }

    /// The length in bytes.
    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.len()
    }

    /// True when there are no bytes.
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Clone for SecretBytes {
    fn clone(&self) -> Self {
        SecretBytes(self.0.clone())
    }
}

impl From<Vec<u8>> for SecretBytes {
    fn from(bytes: Vec<u8>) -> Self {
        SecretBytes(bytes)
    }
}

impl From<&[u8]> for SecretBytes {
    fn from(bytes: &[u8]) -> Self {
        SecretBytes(bytes.to_vec())
    }
}

impl From<String> for SecretBytes {
    fn from(text: String) -> Self {
        SecretBytes(text.into_bytes())
    }
}

/// Read into `bytes`, wiping everything accumulated if the reader fails partway.
#[cfg(any(unix, test))]
pub(crate) fn read_to_end_wiping_on_error(
    mut reader: impl std::io::Read,
    bytes: &mut Vec<u8>,
) -> std::io::Result<usize> {
    match reader.read_to_end(bytes) {
        Ok(count) => Ok(count),
        Err(error) => {
            bytes.zeroize();
            Err(error)
        }
    }
}

impl Drop for SecretBytes {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl std::fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretBytes(<{} bytes redacted>)", self.0.len())
    }
}

#[cfg(test)]
#[path = "secret_tests.rs"]
mod tests;
