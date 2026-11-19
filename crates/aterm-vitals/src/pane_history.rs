// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The local session charts share one host-clock grid. A sweep contributes to the bucket
//! in which it ended; a newer reading in that bucket replaces it, including a missing read.

/// Number of closed five-second buckets in the last minute.
pub const BUCKETS: usize = 12;
/// Width of a bucket on the host's monotonic clock, in nanoseconds.
pub const BUCKET_NS: u64 = 5_000_000_000;
const SLOTS: usize = BUCKETS + 1;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Minute<T> {
    slots: [Option<(u64, T)>; SLOTS],
}

impl<T: Copy> Default for Minute<T> {
    fn default() -> Self {
        Self {
            slots: [None; SLOTS],
        }
    }
}

impl<T: Copy> Minute<T> {
    pub(crate) fn record(&mut self, at_ns: u64, value: Option<T>) {
        if let Some(last_ns) = at_ns.checked_sub(1) {
            let bucket = last_ns / BUCKET_NS;
            self.slots[(bucket % SLOTS as u64) as usize] = value.map(|v| (bucket, v));
        }
    }

    pub(crate) fn buckets(&self, at_ns: u64) -> [Option<T>; BUCKETS] {
        let open = at_ns / BUCKET_NS;
        std::array::from_fn(|index| {
            let bucket = open.checked_sub((BUCKETS - index) as u64)?;
            self.slots[(bucket % SLOTS as u64) as usize]
                .filter(|&(tag, _)| tag == bucket)
                .map(|(_, value)| value)
        })
    }
}
