// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::obs::Source;

/// A login reading: serving, or refused for its credential. `Struck` is the greater, so it
/// decides a tie.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Login {
    Serves,
    Struck,
}

/// 2026-11-13 09:00 UTC, the fixture's strike.
const T: u64 = 1_794_646_800;

fn print(generation: u64) -> CredentialPrint {
    CredentialPrint::new("claude", "m16", generation)
}

/// The "Please run /login" wall on tab `sid`'s row epoch 7, first seen at `at`.
fn wall_in(sid: &str, of: CredentialPrint, at: u64) -> Obs<Login> {
    Obs {
        value: Login::Struck,
        at,
        witness: WitnessId::ScreenLine {
            sid: sid.to_string(),
            hash: 0x5eed,
            row_epoch: 7,
        },
        source: Source::Screen,
        subject: Subject::Credential(of),
    }
}

/// The wall on tab `s-1`.
fn wall(of: CredentialPrint, at: u64) -> Obs<Login> {
    wall_in("s-1", of, at)
}

/// A turn the login served, recorded at `at` as reading `id`.
fn served(of: CredentialPrint, at: u64, id: &str) -> Obs<Login> {
    Obs {
        value: Login::Serves,
        at,
        witness: WitnessId::Reading(id.to_string()),
        source: Source::Record,
        subject: Subject::Credential(of),
    }
}

fn decision(ledger: &Ledger<Login>, of: CredentialPrint) -> Option<(Login, u64)> {
    ledger.verdict(&of).map(|obs| (obs.value, obs.at))
}

/// The incident replayed: a strike from a wall at T and the same login serving at T+k. The newer
/// evidence decides, whichever arrived first; an older reading that arrives later is kept and
/// decides nothing; a newer strike decides again; at one date the greater value decides.
#[test]
fn the_newest_observation_of_the_current_print_decides() {
    let now = print(1);
    let mut ledger = Ledger::new();
    assert_eq!(decision(&ledger, now), None, "no evidence, no verdict");
    assert_eq!(ledger.observe(wall(now, T)), Observed::Decides);
    assert_eq!(decision(&ledger, now), Some((Login::Struck, T)));
    assert_eq!(
        ledger.observe(served(now, T + 600, "turn-1")),
        Observed::Decides
    );
    assert_eq!(
        decision(&ledger, now),
        Some((Login::Serves, T + 600)),
        "a login that served after its strike serves"
    );

    let mut late = Ledger::new();
    assert_eq!(
        late.observe(served(now, T + 600, "turn-1")),
        Observed::Decides
    );
    assert_eq!(
        late.observe(wall(now, T)),
        Observed::Older,
        "the strike arrives after, but is dated before"
    );
    assert_eq!(decision(&late, now), Some((Login::Serves, T + 600)));
    assert_eq!(late.readings_of(&Subject::Credential(now)).count(), 2);

    let newer_strike = Obs {
        witness: WitnessId::TranscriptRow("row-9".to_string()),
        source: Source::Transcript,
        ..wall(now, T + 900)
    };
    assert_eq!(ledger.observe(newer_strike), Observed::Decides);
    assert_eq!(decision(&ledger, now), Some((Login::Struck, T + 900)));

    let mut tie = Ledger::new();
    assert_eq!(tie.observe(served(now, T, "turn-0")), Observed::Decides);
    assert_eq!(tie.observe(wall(now, T)), Observed::Decides);
    assert_eq!(tie.observe(served(now, T, "turn-00")), Observed::Older);
    assert_eq!(decision(&tie, now), Some((Login::Struck, T)));
}

/// Every order of one set of readings gives one verdict, the whole reading: arrival never
/// decides, not even between two readings of one date and value from different witnesses and
/// sources, where the greater witness decides.
#[test]
fn every_arrival_order_gives_one_verdict() {
    let now = print(1);
    let decider = served(now, T + 90, "b");
    let readings = [
        wall(now, T + 60),
        served(now, T + 30, "a"),
        decider.clone(),
        Obs {
            witness: WitnessId::TranscriptRow("c".to_string()),
            source: Source::Transcript,
            ..served(now, T + 90, "c")
        },
        Obs {
            witness: WitnessId::Fleet {
                node: "m8".to_string(),
                seq: 4,
            },
            source: Source::Fleet,
            ..wall(now, T + 20)
        },
    ];
    let mut orders: Vec<Vec<usize>> = vec![vec![]];
    for next in 0..readings.len() {
        orders = orders
            .into_iter()
            .flat_map(|order| {
                (0..=order.len()).map(move |at| {
                    let mut longer = order.clone();
                    longer.insert(at, next);
                    longer
                })
            })
            .collect();
    }
    assert_eq!(orders.len(), 120);
    for order in orders {
        let mut ledger = Ledger::new();
        for &i in &order {
            ledger.observe(readings[i].clone());
        }
        assert_eq!(ledger.verdict(&now), Some(&decider), "{order:?}");
    }
}

/// A wall still on screen at later looks is one witness: each later reading, however it is
/// stamped, whatever it says this time and whichever print it is attributed to, is a re-read that
/// changes nothing, so an old strike re-read after the login served never strikes it again.
#[test]
fn one_witness_read_twice_is_one_observation() {
    let now = print(1);
    let mut ledger = Ledger::new();
    assert_eq!(ledger.observe(wall(now, T)), Observed::Decides);
    assert_eq!(
        ledger.observe(served(now, T + 600, "turn-1")),
        Observed::Decides
    );
    for look in 1..=24 {
        assert_eq!(
            ledger.observe(wall(now, T + 600 + look * 1_800)),
            Observed::Reread,
            "look {look}"
        );
        assert_eq!(decision(&ledger, now), Some((Login::Serves, T + 600)));
    }
    assert_eq!(
        ledger.observe(served(now, T + 50_000, "turn-1")),
        Observed::Reread,
        "a re-read renews nothing, a serving reading included"
    );
    assert_eq!(decision(&ledger, now), Some((Login::Serves, T + 600)));
    let struck_reread = Obs {
        value: Login::Struck,
        ..served(now, T + 600, "turn-1")
    };
    assert_eq!(
        ledger.observe(struck_reread),
        Observed::Reread,
        "the deciding witness read again as struck"
    );
    assert_eq!(
        decision(&ledger, now),
        Some((Login::Serves, T + 600)),
        "a re-read's value replaces nothing, so no stored word comes back through one"
    );
    let serving_wall = Obs {
        value: Login::Serves,
        ..wall(now, T + 60_000)
    };
    assert_eq!(ledger.observe(serving_wall), Observed::Reread);
    assert_eq!(decision(&ledger, now), Some((Login::Serves, T + 600)));
    assert_eq!(
        ledger.observe(wall(print(2), T + 90_000)),
        Observed::Reread,
        "the witness belongs to the subject of its first reading"
    );
    assert_eq!(decision(&ledger, print(2)), None);
    let firsts: Vec<_> = ledger
        .readings_of(&Subject::Credential(now))
        .map(|obs| (obs.value, obs.at))
        .collect();
    assert_eq!(
        firsts,
        [(Login::Struck, T), (Login::Serves, T + 600)],
        "each witness once, with its first date"
    );
}

/// Two tabs on two prints, restarted together, show the same wall at the same row epoch (each
/// terminal counts its own epochs): each line is its own tab's witness, so each reading decides
/// for its own print, whichever tab is read first. The same tab's line offered for the other
/// print is that tab's witness again, a re-read.
#[test]
fn the_same_line_in_two_tabs_is_two_witnesses() {
    let tabs = [
        ("s-1", print(1)),
        ("s-2", CredentialPrint::new("claude", "m8", 1)),
    ];
    for order in [[0, 1], [1, 0]] {
        let mut ledger = Ledger::new();
        for i in order {
            let (sid, of) = tabs[i];
            assert_eq!(
                ledger.observe(wall_in(sid, of, T)),
                Observed::Decides,
                "{sid}, order {order:?}"
            );
        }
        for (sid, of) in tabs {
            assert_eq!(
                decision(&ledger, of),
                Some((Login::Struck, T)),
                "{sid}, order {order:?}"
            );
        }
        let ((one, _), (_, other)) = (tabs[0], tabs[1]);
        assert_eq!(
            ledger.observe(wall_in(one, other, T + 60)),
            Observed::Reread
        );
        assert_eq!(decision(&ledger, other), Some((Login::Struck, T)));
    }
}

/// A renewal is a new print, so it starts with no evidence; a reading of the print it replaced,
/// however new, says nothing of it; nor does any other subject's.
#[test]
fn an_older_prints_observation_says_nothing() {
    let (old, renewed) = (print(1), print(2));
    let mut ledger = Ledger::new();
    assert_eq!(ledger.observe(wall(old, T)), Observed::Decides);
    assert_eq!(decision(&ledger, renewed), None, "a renewal starts empty");
    assert_eq!(
        ledger.observe(served(renewed, T + 60, "turn-1")),
        Observed::Decides
    );
    let late_strike = Obs {
        witness: WitnessId::TranscriptRow("row-2".to_string()),
        ..wall(old, T + 3_600)
    };
    assert_eq!(
        ledger.observe(late_strike),
        Observed::Decides,
        "it decides for the old print"
    );
    assert_eq!(decision(&ledger, old), Some((Login::Struck, T + 3_600)));
    assert_eq!(decision(&ledger, renewed), Some((Login::Serves, T + 60)));
    for other in [
        Subject::Account("m16".to_string()),
        Subject::Process { pid: 41, born: T },
        Subject::Tab("s7".to_string()),
    ] {
        let reading = Obs {
            witness: WitnessId::Reading(format!("{other:?}")),
            subject: other.clone(),
            ..wall(renewed, T + 7_200)
        };
        assert_eq!(ledger.observe(reading), Observed::Decides);
        assert_eq!(ledger.newest_of(&other).map(|obs| obs.at), Some(T + 7_200));
        assert_eq!(decision(&ledger, renewed), Some((Login::Serves, T + 60)));
    }
}
