// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded frame acquisition shared by records transport and local-file recovery.
//! Callers retain their own count, verification and recovery policies.

use super::{MAX_RECORDS_STREAM, Record, Store};
use crate::membership_log::{FRAME_PREFIX_LEN, MAX_RECORDS, RecordId};
use crate::{Error, Result};
use std::collections::BTreeSet;
use std::io::{self, Read};
use zeroize::Zeroizing;

/// A transport read failure differs from an invalid or incomplete frame.
pub(crate) enum FrameReadError {
    Read(io::Error),
    Invalid(Error),
}

/// Read one bounded frame at a time. Callers choose count, verification and recovery policy.
pub(crate) struct FrameReader {
    header: Zeroizing<Vec<u8>>,
}

/// A complete layout frame, with unverified parent identities from its header.
pub(crate) struct LayoutFrame {
    pub(crate) wire: Zeroizing<Vec<u8>>,
    pub(crate) parents: Vec<RecordId>,
}

struct FrameWith<T> {
    wire: Zeroizing<Vec<u8>>,
    info: T,
}

impl FrameReader {
    pub(crate) fn new() -> Self {
        Self {
            header: Zeroizing::new(vec![0; FRAME_PREFIX_LEN]),
        }
    }

    pub(crate) fn read<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
    ) -> std::result::Result<Option<Zeroizing<Vec<u8>>>, FrameReadError> {
        self.read_with(reader, |header| {
            Record::known_frame_header(header).map(|len| (len, ()))
        })
        .map(|frame| frame.map(|frame| frame.wire))
    }

    /// Read this layout without judging its kind. The returned bytes are not verified.
    pub(crate) fn read_layout<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
        remaining: usize,
    ) -> std::result::Result<Option<LayoutFrame>, FrameReadError> {
        self.read_with(reader, |header| {
            let (len, parents) = Record::frame_header(header)?;
            if len > remaining {
                return Err(Error::new("membership record stream is too large"));
            }
            Ok((len, parents))
        })
        .map(|frame| {
            frame.map(|frame| LayoutFrame {
                wire: frame.wire,
                parents: frame.info,
            })
        })
    }

    fn read_with<R: Read + ?Sized, T>(
        &mut self,
        reader: &mut R,
        inspect: impl FnOnce(&[u8]) -> Result<(usize, T)>,
    ) -> std::result::Result<Option<FrameWith<T>>, FrameReadError> {
        let invalid = || FrameReadError::Invalid(Error::new("membership record is truncated"));
        self.header.resize(FRAME_PREFIX_LEN, 0);
        let read = fill(reader, &mut self.header).map_err(FrameReadError::Read)?;
        if read == 0 {
            return Ok(None);
        }
        if read != self.header.len() {
            return Err(invalid());
        }
        let header_len = Record::frame_header_len(&self.header).map_err(FrameReadError::Invalid)?;
        self.header.resize(header_len, 0);
        if fill(reader, &mut self.header[FRAME_PREFIX_LEN..]).map_err(FrameReadError::Read)?
            != header_len - FRAME_PREFIX_LEN
        {
            return Err(invalid());
        }
        let (frame_len, info) = inspect(&self.header).map_err(FrameReadError::Invalid)?;
        let mut frame = Zeroizing::new(vec![0; frame_len]);
        frame[..header_len].copy_from_slice(&self.header);
        if fill(reader, &mut frame[header_len..]).map_err(FrameReadError::Read)?
            != frame_len - header_len
        {
            return Err(invalid());
        }
        Ok(Some(FrameWith { wire: frame, info }))
    }
}

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
        let mut frames = FrameReader::new();
        while records.len() < MAX_RECORDS {
            let frame = match frames.read(&mut reader) {
                Ok(None) => {
                    return Ok(DecodedRecords {
                        records,
                        strict,
                        bytes_read: MAX_RECORDS_STREAM - reader.limit() as usize,
                    });
                }
                Ok(Some(frame)) => frame,
                Err(FrameReadError::Read(error)) => return Err(read_error(error)),
                Err(FrameReadError::Invalid(_)) => {
                    strict = false;
                    break;
                }
            };
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

fn fill<R: Read + ?Sized>(reader: &mut R, bytes: &mut [u8]) -> io::Result<usize> {
    let mut read = 0;
    while read < bytes.len() {
        match reader.read(&mut bytes[read..]) {
            Ok(0) => break,
            Ok(n) => read += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(read)
}

fn read_error(error: io::Error) -> Error {
    Error::new(format!("membership record read failed: {error}"))
}

#[cfg(test)]
mod tests;
