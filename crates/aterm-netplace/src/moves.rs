// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE SETTLED PLACE AS A MOVE SOURCE. A route change is heard at its first message, often the
//! old network's address going away while this Mac is offline, and a source rests after each
//! move it tells, so the arrival on the new network can wake nothing for half a minute. The
//! keeper's record says when the new network is usable: its current stay holds a network id.
//! Every process watches the record ([`SettledWatch`]) and tells a move the moment the place
//! settles onto another network than the one it last told, with no rest; a settle back onto the
//! network it last told (weak Wi-Fi flapping offline and back, a primary interface swapped and
//! back), or onto or off the ALab LAN on it, is told at most once a [`REST_MS`], the route
//! source's rest, and one held back is told when the rest ends. [`crate::place::Record::lone_return`]
//! then says whether that move waits out the herd's spread.

use crate::place::{NetworkId, Record, StayAt};

/// How often a settle back onto the network last told is told again ([`SettledWatch::look`]):
/// the route source's rest (`REST` in aterm-gui), which bounded a flapping link's moves before the
/// place told any.
pub const REST_MS: u64 = 30_000;

/// Where a record's current stay settled: its network, and whether it reads the ALab LAN;
/// `None` while offline or before the stay's network is read.
#[must_use]
pub fn settled(record: &Record) -> Option<(NetworkId, bool)> {
    record.current().and_then(|stay| {
        stay.network
            .clone()
            .map(|network| (network, stay.at == StayAt::AlabLan))
    })
}

/// One process's watch on the keeper's record: whether each look found the place newly settled.
#[derive(Clone, Debug, Default)]
pub struct SettledWatch {
    /// What the last look found; `None` before the first.
    last: Option<Option<(NetworkId, bool)>>,
    /// The network the last move told was on, and when it was told (unix ms).
    told: Option<(NetworkId, u64)>,
    /// A settle held back by the rest, told once the rest ends while the place is still settled.
    held: bool,
}

impl SettledWatch {
    /// A watch that has not looked yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            last: None,
            told: None,
            held: false,
        }
    }

    /// One look at `record` (`None`: unread) at `now_ms`: whether to tell a move. The place
    /// settled since the last look (onto a network from offline, checking or an unread record,
    /// onto another network, or onto or off the ALab LAN on one), and that network is another
    /// than the last told, or the last move was told [`REST_MS`] ago or more; or a settle the rest
    /// held back is due now and the place is still settled. The first look only learns where it
    /// stands.
    pub fn look(&mut self, record: Option<&Record>, now_ms: u64) -> bool {
        let now_ref = record.and_then(Record::current).and_then(|stay| {
            stay.network
                .as_ref()
                .map(|network| (network, stay.at == StayAt::AlabLan))
        });
        let Some((net, is_alab)) = now_ref else {
            self.last = Some(None);
            return false;
        };
        let settled_now = match &mut self.last {
            None => {
                self.last = Some(Some((net.clone(), is_alab)));
                return false;
            }
            Some(before) => {
                let changed = before.as_ref().map(|(n, a)| (n, *a)) != Some((net, is_alab));
                if changed {
                    *before = Some((net.clone(), is_alab));
                }
                changed
            }
        };
        if !settled_now && !self.held {
            return false;
        }
        let rested = self.told.as_ref().is_none_or(|(network, told_ms)| {
            network != net || now_ms.saturating_sub(*told_ms) >= REST_MS
        });
        if rested {
            self.told = Some((net.clone(), now_ms));
            self.held = false;
        } else {
            self.held = true;
        }
        rested
    }
}

#[cfg(test)]
#[path = "moves_tests.rs"]
mod tests;
