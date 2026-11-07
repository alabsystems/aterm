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
mod tests {
    use super::*;
    use crate::place::{Book, Evidence, Facts, Link, PathFacts, RouteFacts, step};
    use std::net::Ipv4Addr;

    const T0: u64 = 1_800_000_000_000;

    fn on(hw: u8, expensive: bool) -> Facts {
        Facts {
            route: Some(RouteFacts {
                interface: "en0".to_owned(),
                address: Ipv4Addr::new(192, 168, 4, 31),
                prefix: 22,
                gateway: Some(Ipv4Addr::new(192, 168, 4, 1)),
                gateway_hw: Some(vec![0, 1, 2, 3, 4, hw]),
            }),
            tunnel: None,
            path: Some(PathFacts {
                usable: Some(true),
                expensive,
                constrained: false,
                link: Link::Wifi,
            }),
        }
    }

    fn offline() -> Facts {
        Facts {
            route: None,
            tunnel: None,
            path: Some(PathFacts {
                usable: Some(false),
                expensive: false,
                constrained: false,
                link: Link::Wifi,
            }),
        }
    }

    /// THE PLACE SETTLING IS A MOVE: from a remote stay through offline onto the remembered ALab
    /// LAN tells one move (when the network is read, not when the link went down), and that move
    /// is a lone return; a router reboot (the ALab LAN, offline, the same ALab LAN) tells one too,
    /// once the network is back, and that one keeps the herd; a look that finds nothing changed,
    /// or the place unsettled, tells nothing.
    /// NEGATIVE CONTROLS: telling a change only between two settled places (the last arm's
    /// `before.as_ref() != Some(&now)` → `before.is_some_and(|before| before != now)`) misses the
    /// arrival after offline, and "home again" fails; `Record::lone_return` without its pass over
    /// the stay the move arrived in reads that arrival as the ALab LAN's, and "a lone return"
    /// fails.
    #[test]
    fn the_place_settling_onto_a_network_is_one_move() {
        let book = Book::default();
        let evidence = Evidence::default();
        let home = on(5, false);
        let (seed, _) = step(
            &Record::default(),
            &home,
            &evidence,
            Some(&book),
            T0,
            Some(T0),
        );
        let mut record = seed;
        record.remembered.push(crate::place::Known {
            network: NetworkId::of(home.route.as_ref().expect("a route")).expect("strong"),
            learned_ms: T0,
            confirmed_ms: T0,
        });
        let mut watch = SettledWatch::new();
        let mut walk = |facts: &Facts, now: u64, record: &mut Record| {
            let (next, _) = step(record, facts, &evidence, Some(&book), now, Some(T0));
            *record = next;
            watch.look(Some(record), now)
        };
        assert!(
            !walk(&home, T0 + 1_000, &mut record),
            "the first look learns"
        );
        assert!(!walk(&home, T0 + 2_000, &mut record), "nothing changed");
        assert!(
            walk(&on(9, false), T0 + 60_000, &mut record),
            "another network"
        );
        let moved_at = T0 + 30 * 60_000;
        assert!(
            !walk(&offline(), moved_at, &mut record),
            "offline is no arrival"
        );
        assert!(
            walk(&home, moved_at + 5_000, &mut record),
            "home again, once its network is read"
        );
        assert!(record.lone_return(moved_at + 5_000), "a lone return");
        assert!(!walk(&home, moved_at + 6_000, &mut record));
        let reboot = moved_at + 60 * 60_000;
        assert!(!walk(&offline(), reboot, &mut record));
        assert!(
            walk(&home, reboot + 30_000, &mut record),
            "back after a reboot"
        );
        assert!(
            !record.lone_return(reboot + 30_000),
            "a reboot keeps the herd"
        );
        assert!(!watch.look(None, reboot), "an unread record tells nothing");
    }

    /// A FLAPPING LINK TELLS AT MOST ONE MOVE A REST: weak Wi-Fi drops to offline and back onto
    /// the same network every few seconds, and only the first settle is told at once; the next
    /// within [`REST_MS`] is held and told when the rest ends, once, while another network is
    /// told at once whatever the rest.
    /// NEGATIVE CONTROL: telling every settle (`rested` always true) tells the flap back within
    /// the rest, and "a flap within the rest tells nothing" fails.
    #[test]
    fn a_flapping_link_tells_at_most_one_move_a_rest() {
        let book = Book::default();
        let evidence = Evidence::default();
        let home = on(5, false);
        let mut record = Record::default();
        let mut watch = SettledWatch::new();
        let mut walk = |facts: &Facts, now: u64, record: &mut Record| {
            let (next, _) = step(record, facts, &evidence, Some(&book), now, Some(T0));
            *record = next;
            watch.look(Some(record), now)
        };
        assert!(!walk(&offline(), T0, &mut record), "the first look learns");
        assert!(walk(&home, T0 + 1_000, &mut record), "the arrival, at once");
        assert!(!walk(&offline(), T0 + 4_000, &mut record));
        assert!(
            !walk(&home, T0 + 6_000, &mut record),
            "a flap within the rest tells nothing"
        );
        assert!(!walk(&home, T0 + 20_000, &mut record), "still resting");
        assert!(
            walk(&home, T0 + 1_000 + REST_MS, &mut record),
            "the held settle, once the rest ends"
        );
        assert!(!walk(&home, T0 + 2_000 + REST_MS, &mut record), "once");
        assert!(
            walk(&on(9, false), T0 + 3_000 + REST_MS, &mut record),
            "another network, at once"
        );
    }
}
