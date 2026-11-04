// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Immutable ciphertext bytes, owned directly by a draft or shared with a signed record.

use crate::{Error, Result};
use std::fmt;
use std::ops::{Deref, Range};
use std::sync::Arc;

/// Immutable ciphertext. A decoded payload shares its signed record's backing bytes; cloning
/// it keeps those bytes alive independently of the record or the caller's input buffer.
#[derive(Clone)]
pub struct Payload {
    bytes: Arc<[u8]>,
    range: Range<usize>,
}

impl Payload {
    pub(super) fn in_wire(bytes: &Arc<[u8]>, range: Range<usize>) -> Result<Self> {
        if bytes.get(range.clone()).is_none() {
            return Err(Error::new(
                "membership payload is outside its signed record",
            ));
        }
        Ok(Self {
            bytes: Arc::clone(bytes),
            range,
        })
    }
}

impl From<Vec<u8>> for Payload {
    fn from(bytes: Vec<u8>) -> Self {
        let range = 0..bytes.len();
        Self {
            bytes: Arc::from(bytes),
            range,
        }
    }
}

impl Deref for Payload {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.bytes[self.range.clone()]
    }
}

impl AsRef<[u8]> for Payload {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

impl fmt::Debug for Payload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_ref(), f)
    }
}
