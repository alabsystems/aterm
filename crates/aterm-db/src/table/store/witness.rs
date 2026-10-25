// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A custody-side commitment to every acknowledged local receipt. The astream
//! checkpoint lives with the data; this witness survives restoring those files.

use crate::{Error, Result};
use astream_wire::sync::Digest;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Witness {
    pub count: u64,
    hash: Digest,
}
impl Witness {
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let count = u64::from_le_bytes(bytes[..8].try_into().unwrap());
        let hash = bytes[8..].try_into().unwrap();
        if count > super::JOURNAL_RECORDS as u64 || (count == 0 && hash != [0; 32]) {
            return Err(Error::new("invalid table custody witness"));
        }
        Ok(Self { count, hash })
    }
    pub fn bytes(self) -> [u8; 40] {
        let mut bytes = [0; 40];
        bytes[..8].copy_from_slice(&self.count.to_le_bytes());
        bytes[8..].copy_from_slice(&self.hash);
        bytes
    }
    pub fn advance(self, offset: u64, id: Digest, received_ms: u64) -> Result<Self> {
        if offset != self.count || self.count >= super::JOURNAL_RECORDS as u64 {
            return Err(Error::new("table custody witness is not contiguous"));
        }
        let mut hash = aterm_digest::Sha256::new();
        hash.update(b"aterm/commons/table-receipts/1\0");
        hash.update(self.hash);
        hash.update(offset.to_le_bytes());
        hash.update(id);
        hash.update(received_ms.to_le_bytes());
        Ok(Self {
            count: self.count + 1,
            hash: hash.finalize(),
        })
    }
}
