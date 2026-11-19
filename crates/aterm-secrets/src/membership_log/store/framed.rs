// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded frame acquisition shared by records transport and local-file recovery.
//! Callers retain their own count, verification and recovery policies.

use super::{Reading, Record, Store};
use crate::membership_log::{FRAME_PREFIX_LEN, RecordId};
use crate::{Error, Result};
#[cfg(any(unix, test))]
use std::collections::BTreeSet;
use std::io::{self, Read};
use zeroize::Zeroizing;

/// A transport read failure differs from an invalid or incomplete frame.
pub(crate) enum FrameReadError {
    Read(io::Error),
    Invalid(
        #[cfg_attr(
            not(unix),
            expect(
                dead_code,
                reason = "only the unix vault server answers with the reason"
            )
        )]
        Error,
    ),
}

/// Read one bounded frame at a time. Callers choose count, verification and recovery policy.
pub(crate) struct FrameReader {
    header: Zeroizing<Vec<u8>>,
    /// The layouts it frames ([`Reading`]).
    reading: Reading,
    /// How many bytes of `header` the last read took before it refused them as no record's
    /// prefix ([`Self::refused_prefix`]); 0 after any other read.
    refused: usize,
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
    /// A reader of the layouts `reading` reads.
    pub(crate) fn at(reading: Reading) -> Self {
        Self {
            header: Zeroizing::new(vec![0; FRAME_PREFIX_LEN]),
            reading,
            refused: 0,
        }
    }

    /// The bytes the last read took from its reader and refused as no record's prefix: fewer
    /// than a prefix, or a prefix of no layout this reader frames. They are the first bytes of
    /// whatever follows the records in the stream, which a reader that takes a served frame
    /// there reads on from (`untaken::ReceivedFrames::read_at`). Empty after a read that
    /// returned a frame, reached the end, or refused a frame past its prefix.
    pub(crate) fn refused_prefix(&self) -> &[u8] {
        &self.header[..self.refused]
    }

    pub(crate) fn read<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
    ) -> std::result::Result<Option<Zeroizing<Vec<u8>>>, FrameReadError> {
        let reading = self.reading;
        self.read_with(reader, |header| {
            Record::known_frame_header(header, reading).map(|len| (len, ()))
        })
        .map(|frame| frame.map(|frame| frame.wire))
    }

    /// Read this layout without judging its kind. The returned bytes are not verified.
    pub(crate) fn read_layout<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
        remaining: usize,
    ) -> std::result::Result<Option<LayoutFrame>, FrameReadError> {
        let reading = self.reading;
        self.read_with(reader, |header| {
            let (len, parents) = Record::frame_header(header, reading)?;
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
        self.refused = 0;
        self.header.resize(FRAME_PREFIX_LEN, 0);
        let read = fill(reader, &mut self.header).map_err(FrameReadError::Read)?;
        if read == 0 {
            return Ok(None);
        }
        if read != self.header.len() {
            self.refused = read;
            return Err(invalid());
        }
        let header_len = match Record::frame_header_len(&self.header, self.reading) {
            Ok(len) => len,
            Err(refused) => {
                self.refused = read;
                return Err(FrameReadError::Invalid(refused));
            }
        };
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
    #[cfg_attr(
        all(not(unix), not(test)),
        expect(
            dead_code,
            reason = "only the unix vault server reads how much it read"
        )
    )]
    bytes_read: usize,
}

impl DecodedRecords {
    #[cfg(any(unix, test))]
    pub(crate) const fn bytes_read(&self) -> usize {
        self.bytes_read
    }

    pub(crate) fn apply(self, store: Store) -> Store {
        store.recover_decoded(self.records, self.strict)
    }

    /// An unrooted server keeps new records only if they establish a genesis.
    /// While no genesis lands, insertion can only add unrooted IDs, never replace them.
    /// Retain the original IDs on that outcome without cloning their Store or bytes.
    #[cfg(any(unix, test))]
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
    /// Recover from the first bytes of a local-file reader that a store of this format holds
    /// ([`Store::capacity`], the storage class of byte bound). I/O errors refuse the whole read;
    /// malformed record tails retain the same prefix as [`Store::recovering`], within this
    /// store's held budget of verified frames. Every byte of the capped prefix is consumed,
    /// including after recovery stops, for its complete digest.
    pub(crate) fn recovering_read(self, reader: impl Read) -> Result<Self> {
        let reading = self.reading;
        Ok(Self::read_records_bounded(reader, self.budgets.held, reading)?.apply(self))
    }

    /// Read capped recovery input at the receiving store's explicit record limit, each frame
    /// as `reading` reads it (the format the receiving store's node reads its records at). A
    /// server finishes its whole-file length and descriptor checks before applying this batch.
    #[cfg(any(unix, test))]
    pub(crate) fn read_records(
        reader: impl Read,
        record_limit: usize,
        reading: Reading,
    ) -> Result<DecodedRecords> {
        Self::read_records_bounded(reader, record_limit, reading)
    }

    fn read_records_bounded(
        reader: impl Read,
        record_limit: usize,
        reading: Reading,
    ) -> Result<DecodedRecords> {
        let capacity = reading.capacity();
        let mut reader = reader.take(capacity as u64);
        let mut records = Vec::new();
        let mut strict = true;
        let mut frames = FrameReader::at(reading);
        while records.len() < record_limit {
            let frame = match frames.read(&mut reader) {
                Ok(None) => {
                    return Ok(DecodedRecords {
                        records,
                        strict,
                        bytes_read: capacity - reader.limit() as usize,
                    });
                }
                Ok(Some(frame)) => frame,
                Err(FrameReadError::Read(error)) => return Err(read_error(error)),
                Err(FrameReadError::Invalid(_)) => {
                    strict = false;
                    break;
                }
            };
            match Record::decode_in(&frame, reading) {
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
            bytes_read: capacity - reader.limit() as usize,
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
