// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded frames for the existing local-file recovery policy, not archive completeness.

use super::{MAX_RECORDS_STREAM, Record, Store};
use crate::membership_log::{FRAME_PREFIX_LEN, MAX_RECORDS};
use crate::{Error, Result};
use std::collections::BTreeSet;
use std::io::{self, Read};
use zeroize::Zeroizing;

/// Fully read verified frames. Applying this batch performs no I/O.
pub(crate) struct DecodedRecords {
    records: Vec<Record>,
    strict: bool,
    bytes_read: usize,
}

impl DecodedRecords {
    pub(crate) const fn bytes_read(&self) -> usize {
        self.bytes_read
    }

    pub(crate) fn apply(self, store: Store) -> Store {
        store.recover_decoded(self.records, self.strict)
    }

    /// An unrooted server keeps new records only if they establish a genesis.
    /// While no genesis lands, insertion can only add unrooted IDs, never replace them.
    /// Retain the original IDs on that outcome without cloning their Store or bytes.
    pub(crate) fn apply_rooted(self, store: Store) -> Store {
        let original: Option<BTreeSet<_>> = store
            .log
            .is_none()
            .then(|| store.unrooted.keys().copied().collect());
        let mut recovered = self.apply(store);
        if let Some(original) = original
            && recovered.log.is_none()
        {
            recovered.unrooted.retain(|id, _| original.contains(id));
        }
        recovered
    }
}

impl Store {
    /// Recover from a local-file reader's first [`MAX_RECORDS_STREAM`] bytes. I/O errors refuse the whole read;
    /// malformed record tails retain the same prefix as [`Store::recovering`]. Every byte of
    /// the capped prefix is consumed, including after recovery stops, for its complete digest.
    pub(crate) fn recovering_read(self, reader: impl Read) -> Result<Self> {
        Ok(Self::read_records(reader)?.apply(self))
    }

    /// Read the same capped recovery input without changing a Store. A server can finish
    /// its whole-file length and descriptor checks before applying this batch.
    pub(crate) fn read_records(reader: impl Read) -> Result<DecodedRecords> {
        let mut reader = reader.take(MAX_RECORDS_STREAM as u64);
        let mut records = Vec::new();
        let mut strict = true;
        let mut header = Zeroizing::new(vec![0; FRAME_PREFIX_LEN]);
        while records.len() < MAX_RECORDS {
            header.resize(FRAME_PREFIX_LEN, 0);
            let read = fill(&mut reader, &mut header)?;
            if read == 0 {
                return Ok(DecodedRecords {
                    records,
                    strict,
                    bytes_read: MAX_RECORDS_STREAM - reader.limit() as usize,
                });
            }
            if read != header.len() {
                strict = false;
                break;
            }
            let Ok(header_len) = Record::frame_header_len(&header) else {
                strict = false;
                break;
            };
            header.resize(header_len, 0);
            if fill(&mut reader, &mut header[FRAME_PREFIX_LEN..])? != header_len - FRAME_PREFIX_LEN
            {
                strict = false;
                break;
            }
            let Ok(frame_len) = Record::known_frame_header(&header) else {
                strict = false;
                break;
            };
            let mut frame = Zeroizing::new(vec![0; frame_len]);
            frame[..header_len].copy_from_slice(&header);
            if fill(&mut reader, &mut frame[header_len..])? != frame_len - header_len {
                strict = false;
                break;
            }
            match Record::decode(&frame) {
                Ok(record) => records.push(record),
                Err(_) => strict = false,
            }
        }
        let mut tail = Zeroizing::new([0; 16 * 1024]);
        loop {
            match reader.read(&mut *tail) {
                Ok(0) => break,
                Ok(_) => strict = false,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(read_error(e)),
            }
        }
        Ok(DecodedRecords {
            records,
            strict,
            bytes_read: MAX_RECORDS_STREAM - reader.limit() as usize,
        })
    }
}

fn fill(reader: &mut impl Read, bytes: &mut [u8]) -> Result<usize> {
    let mut read = 0;
    while read < bytes.len() {
        match reader.read(&mut bytes[read..]) {
            Ok(0) => break,
            Ok(n) => read += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(read_error(e)),
        }
    }
    Ok(read)
}

fn read_error(error: io::Error) -> Error {
    Error::new(format!("membership record read failed: {error}"))
}

#[cfg(test)]
mod tests;
