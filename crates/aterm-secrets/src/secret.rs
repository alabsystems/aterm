// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`SecretBytes`]: an owned secret that is wiped when dropped and never printed.

use zeroize::Zeroize;

/// Bytes that are a secret (a value, a key, a plaintext): wiped on drop, `Debug` shows only
/// the length, and there is no `Display`. A move can leave a copy behind on the stack; this
/// wipes the one the program keeps.
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
/// Success transfers the bytes to the caller unchanged; callers still impose their cap.
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
mod tests {
    use super::{SecretBytes, read_to_end_wiping_on_error};
    use zeroize::Zeroize;

    #[test]
    fn debug_never_shows_the_bytes() {
        let secret = SecretBytes::from(b"hunter2".as_slice());
        let shown = format!("{secret:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(shown.contains("7 bytes"), "{shown}");
    }

    #[test]
    fn a_partial_read_error_wipes_the_accumulated_bytes() {
        use std::cell::Cell;
        use std::io::{self, Read};

        struct PartialThenError<'a> {
            remaining: &'static [u8],
            sent: &'a Cell<usize>,
        }
        impl Read for PartialThenError<'_> {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                if self.remaining.is_empty() {
                    return Err(io::Error::other("captured read failure"));
                }
                let count = out.len().min(self.remaining.len());
                out[..count].copy_from_slice(&self.remaining[..count]);
                self.remaining = &self.remaining[count..];
                self.sent.set(self.sent.get() + count);
                Ok(count)
            }
        }

        let sent = Cell::new(0);
        let mut bytes = b"existing".to_vec();
        let failed = read_to_end_wiping_on_error(
            PartialThenError {
                remaining: b"partial",
                sent: &sent,
            },
            &mut bytes,
        )
        .expect_err("the reader fails after supplying bytes");
        assert_eq!(sent.get(), b"partial".len());
        assert_eq!(failed.kind(), io::ErrorKind::Other);
        assert!(bytes.is_empty(), "no accumulated bytes survive the error");

        assert_eq!(
            read_to_end_wiping_on_error(b"complete".as_slice(), &mut bytes).expect("complete"),
            8
        );
        assert_eq!(bytes, b"complete");
        assert_eq!(
            read_to_end_wiping_on_error(b"".as_slice(), &mut bytes).expect("empty"),
            0
        );
        assert_eq!(
            bytes, b"complete",
            "a successful empty read leaves existing bytes"
        );
        bytes.zeroize();
    }
}
