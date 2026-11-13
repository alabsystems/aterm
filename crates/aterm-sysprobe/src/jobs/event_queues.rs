// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Complete bounded queue evidence, independent of unrelated descriptor-detail costs.

use super::{FdKind, FdReading, Knote, MAX_FD_DETAILS, MAX_FDS, MAX_KNOTES};

/// libproc maps a syscall failure to zero; a cleared and immediately captured errno must
/// accompany each result. Byte-list flavors must also return whole records within the buffer.
pub(super) fn complete_bytes(
    got: i32,
    error: i32,
    width: usize,
    slots: usize,
    limit: usize,
) -> Option<usize> {
    let bytes = usize::try_from(got).ok()?;
    if error != 0 || width == 0 || bytes % width != 0 {
        return None;
    }
    let count = bytes / width;
    (count < slots && count <= limit).then_some(count)
}

/// EXTINFO returns a count of knotes, not bytes, and may report more than the supplied slots.
pub(super) fn complete_notes(got: i32, error: i32) -> Option<usize> {
    let count = usize::try_from(got).ok()?;
    (error == 0 && count < MAX_KNOTES).then_some(count)
}

pub(super) fn exact_record(got: i32, error: i32, bytes: usize) -> bool {
    error == 0 && bytes > 0 && usize::try_from(got).ok() == Some(bytes)
}

/// An empty note result is meaningful only after a full successful kqueue-info read.
pub(super) fn known_queue(
    fd: i32,
    sleeping: Option<bool>,
    notes: impl FnOnce() -> Option<Vec<Knote>>,
) -> Option<FdReading> {
    let sleeping = sleeping?;
    let notes = notes()?;
    (notes.len() < MAX_KNOTES).then_some(FdReading {
        fd,
        kind: FdKind::EventQueue {
            sleeping: Some(sleeping),
            knotes: Some(notes),
        },
    })
}

pub(super) fn read_complete(
    listed: &[(i32, bool)],
    cut: bool,
    mut read: impl FnMut(i32) -> Option<FdReading>,
) -> Option<Vec<FdReading>> {
    if cut || listed.len() > MAX_FDS {
        return None;
    }
    let queues: Vec<_> = listed
        .iter()
        .filter_map(|(fd, queue)| queue.then_some(*fd))
        .collect();
    if queues.len() > MAX_FD_DETAILS {
        return None;
    }
    let mut observed = Vec::with_capacity(queues.len());
    for fd in queues {
        let queue = read(fd)?;
        if queue.fd != fd
            || !matches!(&queue.kind,
            FdKind::EventQueue { knotes: Some(notes), .. } if notes.len() < MAX_KNOTES)
        {
            return None;
        }
        observed.push(queue);
    }
    Some(observed)
}

#[cfg(test)]
#[path = "event_queues_tests.rs"]
mod tests;
