// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One native observation's borrowed session counters, indexed before any of it is adopted.

use std::collections::HashMap;

use aterm_resource_host::resource_observation::SessionUsage;

pub(super) struct Index<'a> {
    sessions: HashMap<&'a str, &'a SessionUsage>,
}

impl<'a> Index<'a> {
    /// Index every receipt once. Duplicate sessions make the observation ambiguous.
    pub(super) fn new(receipts: &'a [SessionUsage]) -> Option<Self> {
        let mut sessions = HashMap::with_capacity(receipts.len());
        for receipt in receipts {
            if sessions.insert(receipt.session.as_str(), receipt).is_some() {
                return None;
            }
        }
        Some(Self { sessions })
    }

    /// A same-host observation must retain every known session and both monotone counters.
    pub(super) fn regresses(&self, previous: &HashMap<String, (u64, u64)>) -> bool {
        previous.iter().any(|(session, &(cpu, unread))| {
            self.sessions
                .get(session.as_str())
                .is_none_or(|receipt| receipt.reaped_cpu_ns < cpu || receipt.unread < unread)
        })
    }
}
