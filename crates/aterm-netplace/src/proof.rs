// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ALAB LAN BY PROOF, NOW, for a caller that must not take memory for proof: the vault's
//! automatic ALab LAN admission (`aterm_secrets::home_lan`). The place reads `alab-lan` for a
//! network this Mac remembers (members answered there once) or the fleet book seeds, which is
//! enough to word a report and never enough to let a Mac in.
//!
//! [`Now`] is what this Mac reads now, from the same files and the same Mac as the place: the
//! network its traffic leaves on, with the path this process read itself
//! ([`crate::facts::here_now`]: the Mac's own facts, a test's thread script, never the lab's seam
//! file); the current stay on it, as the keeper's record brought up to
//! those facts gives it (a record that lags a move, or none at all, opens a stay now, in which
//! nothing has answered yet); and the members' authenticated answers ([`crate::heard`]), each
//! with the dial's start, the local address and interface its route took, and when it came. The
//! rules a caller holds that to (which lane, how fresh, which interface, which gateway) stay the
//! caller's: nothing here decides, and nothing here writes.

use std::net::IpAddr;
use std::path::Path;

use crate::facts::Here;
use crate::place::{Answer, Facts, NetworkId, step};

/// What this Mac reads of its network now ([the module](self)).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Now {
    /// The route traffic leaves on (under a tunnel, the physical one), the tunnel holding the
    /// default route, and the path as this process read it now (`None`: not read).
    pub facts: Facts,
    /// The route's interface is point to point.
    pub point_to_point: bool,
    /// Every address this Mac holds, on any interface.
    pub own: Vec<IpAddr>,
    /// When the current stay began (unix ms); `None` with no stay.
    pub stay_since_ms: Option<u64>,
    /// The current stay's network: strong once its gateway's hardware address was read
    /// ([`NetworkId::of`]), weak after the settle without it, `None` before.
    pub stay_network: Option<NetworkId>,
    /// The members' authenticated answers, on every lane, as the evidence file holds them (the
    /// newest per member and lane); none where the file holds a line this build cannot read, so
    /// a torn or corrupted file proves nothing rather than the lines around the damage.
    pub answers: Vec<Answer>,
    /// When it was read (unix ms).
    pub at_ms: u64,
}

/// [`Now`] under the state root `state` (the place's record and evidence there), from `here` at
/// `now`, writing nothing.
#[must_use]
pub fn read_with(state: &Path, here: Here, now: u64) -> Now {
    let dir = crate::record::dir(state);
    let (record, (evidence, passed)) = match aterm_uds::private_file::JudgedDir::judge(&dir) {
        Ok(judged) => (
            crate::record::read_in(&judged)
                .map(|written| written.record)
                .unwrap_or_default(),
            crate::heard::read_counted_in(&dir, &judged),
        ),
        Err(_) => (
            Default::default(),
            (
                Default::default(),
                crate::heard::refused(&dir, crate::heard::FILE),
            ),
        ),
    };
    let (record, _) = step(&record, &here.facts, &evidence, None, now, None);
    let stay = record.current();
    Now {
        stay_since_ms: stay.map(|stay| stay.start_ms),
        stay_network: stay.and_then(|stay| stay.network.clone()),
        answers: if passed == 0 {
            evidence.answers
        } else {
            Vec::new()
        },
        facts: here.facts,
        point_to_point: here.point_to_point,
        own: here.own,
        at_ms: now,
    }
}

/// [`read_with`] over this Mac now ([`crate::facts::here_now`]) at this clock. It reads
/// Network.framework's path, so never on a window's main thread.
#[must_use]
pub fn read(state: &Path) -> Now {
    read_with(state, crate::facts::here_now(), crate::unix_ms_now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::place::{Lane, Link, PathFacts, Record, RouteFacts, member_key};
    use crate::record::Written;
    use std::net::Ipv4Addr;

    const T0: u64 = 1_800_000_000_000;

    /// This Mac on `192.168.4.31/22` behind the router whose hardware address ends in `hw`.
    fn on(hw: u8) -> Here {
        Here {
            facts: Facts {
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
                    expensive: false,
                    constrained: false,
                    link: Link::Wifi,
                }),
            },
            point_to_point: false,
            own: vec![IpAddr::V4(Ipv4Addr::new(192, 168, 4, 31))],
        }
    }

    /// The stay a proof reads is the keeper's record brought up to the facts now: the record's
    /// own while the route continues it, and one opened now where the record lags a move (the
    /// keeper has not looked since this Mac came onto a lookalike network behind another router)
    /// or there is no record, so nothing heard before counts as heard in it. The answers come
    /// back as the evidence file holds them, and nothing is written.
    /// NEGATIVE CONTROL: taking the record's current stay as it stands (`record.current()` of the
    /// record read, not of `step`'s) gives no stay where there is no record, and "no record: a
    /// stay opened now" fails.
    #[test]
    fn the_stay_is_the_records_brought_up_to_the_facts_now() {
        let root = aterm_tempfile::tempdir().expect("a scratch directory");
        let dir = crate::record::dir(root.path());
        let fresh = read_with(root.path(), on(5), T0);
        assert_eq!(
            fresh.stay_since_ms,
            Some(T0),
            "no record: a stay opened now"
        );
        let (record, _) = step(
            &Record::default(),
            &on(5).facts,
            &Default::default(),
            None,
            T0,
            Some(T0),
        );
        crate::record::write(
            &dir,
            &Written {
                revision: 1,
                keeper_pid: Some(std::process::id()),
                keeper_started_ms: Some(T0),
                written_ms: T0,
                record,
            },
        )
        .expect("the record");
        crate::heard::add(
            &dir,
            crate::place::Evidence {
                answers: vec![Answer {
                    lane: Lane::Fleet,
                    member: member_key(Lane::Fleet, "m1"),
                    addr: Some(IpAddr::V4(Ipv4Addr::new(192, 168, 4, 9))),
                    via: Some(Ipv4Addr::new(192, 168, 4, 31)),
                    iface: Some("en0".to_owned()),
                    network: on(5).facts.route.as_ref().and_then(NetworkId::of),
                    logged: true,
                    began_ms: T0 + 1_000,
                    at_ms: T0 + 1_500,
                }],
                ..Default::default()
            },
        )
        .expect("the evidence");
        let held = std::fs::read(dir.join(crate::record::FILE)).expect("the record file");
        let home = read_with(root.path(), on(5), T0 + 60_000);
        assert_eq!(home.stay_since_ms, Some(T0), "the record's stay continues");
        assert_eq!(
            home.stay_network,
            on(5).facts.route.as_ref().and_then(NetworkId::of)
        );
        assert_eq!(home.answers.len(), 1);
        assert_eq!(home.answers[0].iface.as_deref(), Some("en0"));
        let lookalike = read_with(root.path(), on(9), T0 + 120_000);
        assert_eq!(
            (lookalike.stay_since_ms, lookalike.stay_network.clone()),
            (
                Some(T0 + 120_000),
                on(9).facts.route.as_ref().and_then(NetworkId::of)
            ),
            "a lagging record's stay is not this network's"
        );
        assert_eq!(
            std::fs::read(dir.join(crate::record::FILE)).expect("the record file"),
            held,
            "nothing written"
        );
    }

    /// An evidence file with a line this build cannot read proves nothing: a proof reads no
    /// answers from it, not the lines around the damage, while the place's own reports still read
    /// what they can (`heard::read`).
    /// NEGATIVE CONTROL: `read_with` taking `evidence.answers` whatever `passed` says reads the
    /// answers of the whole lines beside the torn one, and this test fails at "a torn file
    /// proves nothing".
    #[test]
    fn a_torn_evidence_file_proves_nothing() {
        let root = aterm_tempfile::tempdir().expect("a scratch directory");
        let dir = crate::record::dir(root.path());
        let heard = |member: &str| Answer {
            lane: Lane::Fleet,
            member: member_key(Lane::Fleet, member),
            addr: Some(IpAddr::V4(Ipv4Addr::new(192, 168, 4, 9))),
            via: Some(Ipv4Addr::new(192, 168, 4, 31)),
            iface: Some("en0".to_owned()),
            network: on(5).facts.route.as_ref().and_then(NetworkId::of),
            logged: true,
            began_ms: T0 + 1_000,
            at_ms: T0 + 1_500,
        };
        crate::heard::add(
            &dir,
            crate::place::Evidence {
                answers: vec![heard("m1"), heard("m2")],
                ..Default::default()
            },
        )
        .expect("the evidence");
        assert_eq!(
            read_with(root.path(), on(5), T0 + 60_000).answers.len(),
            2,
            "PRECONDITION: a whole file proves its answers"
        );
        let file = dir.join(crate::heard::FILE);
        let mut text = std::fs::read_to_string(&file).expect("the evidence file");
        let first = text.lines().next().expect("a line").to_owned();
        text.push_str(&first[..first.len() / 2]);
        text.push('\n');
        aterm_uds::private_file::write_private(&dir, crate::heard::FILE, text.as_bytes())
            .expect("the torn file");
        assert_eq!(
            read_with(root.path(), on(5), T0 + 60_000).answers,
            Vec::new(),
            "a torn file proves nothing"
        );
        assert_eq!(
            crate::heard::read(&dir).answers.len(),
            2,
            "the reports still read the whole lines"
        );
    }
}
