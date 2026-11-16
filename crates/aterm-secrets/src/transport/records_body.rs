// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A bounded wire reader over one immutable record selection.

use crate::membership_log::{MAX_RECORDS_STREAM, Record};
use crate::{Error, Result};
use std::io::{self, Read};

pub(crate) struct RecordsReader<'a> {
    records: &'a [Record],
    len: usize,
    next: usize,
    offset: usize,
    /// The bytes of `records[next]`, read for this send alone: a record an open through the
    /// vault's head shelved is read back once for it and not kept
    /// ([`Record::wire_bytes`]).
    current: Option<std::borrow::Cow<'a, [u8]>>,
}

impl<'a> RecordsReader<'a> {
    pub(crate) fn new(records: &'a [Record]) -> Result<Self> {
        let len = records.iter().try_fold(0usize, |len, record| {
            len.checked_add(record.wire_len())
                .filter(|len| *len <= MAX_RECORDS_STREAM)
        });
        let len = len.ok_or_else(|| {
            Error::new(format!(
                "the record batch is larger than {MAX_RECORDS_STREAM} bytes"
            ))
        })?;
        Ok(Self {
            records,
            len,
            next: 0,
            offset: 0,
            current: None,
        })
    }

    pub(crate) const fn len(&self) -> usize {
        self.len
    }
}

impl Read for RecordsReader<'_> {
    /// # Errors
    /// A shelved record does not read back as it was: the body ends short of its length.
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let mut copied = 0;
        while copied < output.len() {
            let Some(record) = self.records.get(self.next) else {
                break;
            };
            let current = match &mut self.current {
                Some(current) => current,
                empty => empty.insert(record.wire_bytes()?),
            };
            let bytes = &current[self.offset..];
            let take = bytes.len().min(output.len() - copied);
            output[copied..copied + take].copy_from_slice(&bytes[..take]);
            copied += take;
            self.offset += take;
            if self.offset == current.len() {
                self.next += 1;
                self.offset = 0;
                self.current = None;
            }
        }
        Ok(copied)
    }
}

#[cfg(test)]
#[path = "records_body_tests.rs"]
mod tests;
