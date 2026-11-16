// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The infallible encoders this crate's tests write and read records with. They read a shelved
//! record's bytes back and keep them, and give empty bytes where it does not read back: no build
//! without tests has them, so every encoder a build sends or writes by is fallible
//! ([`super::read_records`], [`Store::read_encoded`], [`Store::read_held`],
//! [`Record::wire_bytes`]), and a test that reads a shelved record's bytes asks for what it means
//! to.

use super::{Record, RecordId, Store};

/// [`super::read_records`] of records in memory.
pub(crate) fn encode_records<'a>(records: impl IntoIterator<Item = &'a Record>) -> Vec<u8> {
    let mut out = Vec::new();
    for record in records {
        out.extend_from_slice(record.bytes());
    }
    out
}

impl Store {
    /// [`Store::read_encoded`], reading a shelved record back and keeping it.
    pub(crate) fn encode(&self) -> Vec<u8> {
        self.encoded_records()
            .flat_map(Record::bytes)
            .copied()
            .collect()
    }

    /// [`Store::read_held`], reading a shelved record back and keeping it.
    #[cfg(unix)] // read by the unix-only vault client's tests
    pub(crate) fn encode_held(&self) -> Vec<u8> {
        self.encode_held_from(0)
    }

    /// [`Store::write_held_from`]'s bytes, reading a shelved record back and keeping it.
    pub(crate) fn encode_held_from(&self, from: usize) -> Vec<u8> {
        self.held_from(from)
            .iter()
            .flat_map(Record::bytes)
            .copied()
            .collect()
    }

    /// The exact signed bytes of `id` when held, complete or waiting ([`Store::record`]).
    pub(crate) fn get_bytes(&self, id: &RecordId) -> Option<&[u8]> {
        self.record(id).map(Record::bytes)
    }

    /// The exact signed bytes of `id` when held complete ([`Store::holds`]).
    pub(crate) fn complete_bytes(&self, id: &RecordId) -> Option<&[u8]> {
        self.log()?.get(id).map(Record::bytes)
    }
}

impl Record {
    /// The exact signed bytes ([`Record::wire_bytes`]): a shelved record's read back from its
    /// records file the first time they are asked for, and kept, and empty where that read fails.
    pub(crate) fn bytes(&self) -> &[u8] {
        self.wire.bytes()
    }
}
