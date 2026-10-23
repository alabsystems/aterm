// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Round-trip, byte-layout, and validation tests for cursor-momentum snapshots.

use std::time::Duration;

use aterm_render::GlowQuad;
use aterm_time::Instant;

use super::*;
use crate::cursor_momentum::test_fixtures::{
    Clock, Ev, MS, Obs, TAU, assert_admitted, config, fixtures, light_page_burst, padded, plain,
    step,
};
use crate::cursor_momentum::{LightInkMemo, MomentumGlowConfig, MomentumGlowFrame};

/// A straight run: the observation of every input, and the engine and its
/// snapshot at every boundary (`engines[k]` is the state before input `k`).
struct Straight {
    obs: Vec<Obs>,
    engines: Vec<MomentumGlow>,
    snaps: Vec<MomentumGlowSnapshot>,
}

fn straight(evs: &[Ev], clock: &Clock, epoch: Instant) -> Straight {
    let mut g = MomentumGlow::default();
    let mut run = Straight {
        obs: Vec::with_capacity(evs.len()),
        engines: vec![g],
        snaps: vec![g.snapshot(epoch).expect("a live engine snapshots")],
    };
    for ev in evs {
        run.obs.push(step(&mut g, clock, ev));
        run.engines.push(g);
        run.snaps
            .push(g.snapshot(epoch).expect("a live engine snapshots"));
    }
    run
}

/// Continues `g` over `evs`.
fn resume(mut g: MomentumGlow, evs: &[Ev], clock: &Clock) -> (Vec<Obs>, MomentumGlow) {
    let obs = evs.iter().map(|ev| step(&mut g, clock, ev)).collect();
    (obs, g)
}

/// The first input whose observation differs, if any.
fn first_divergence(a: &[Obs], b: &[Obs]) -> Option<usize> {
    if a.len() != b.len() {
        return Some(a.len().min(b.len()));
    }
    a.iter().zip(b).position(|(x, y)| x != y)
}

/// Snapshot → bytes → snapshot → engine.
fn through_bytes(s: &MomentumGlowSnapshot, epoch: Instant) -> MomentumGlow {
    let decoded = MomentumGlowSnapshot::decode(&s.encode()).expect("an encoding decodes");
    assert_eq!(decoded, *s, "decode(encode(s)) is s");
    MomentumGlow::restore(&decoded, epoch)
        .unwrap_or_else(|e| panic!("a reached state restores: {e}"))
}

#[test]
fn a_snapshot_round_trips_through_its_bytes_at_every_event_boundary_and_continues_the_straight_run()
{
    for (name, evs) in fixtures() {
        assert_admitted(&evs);
        let clock = Clock::ahead(Duration::ZERO);
        let run = straight(&evs, &clock, clock.origin);
        for k in 0..=evs.len() {
            let g = through_bytes(&run.snaps[k], clock.origin);
            assert_eq!(
                g.snapshot(clock.origin),
                Ok(run.snaps[k]),
                "{name}: the restored state is the snapshot's"
            );
            let (obs, end) = resume(g, &evs[k..], &clock);
            assert_eq!(
                first_divergence(&obs, &run.obs[k..]),
                None,
                "{name}: restored at boundary {k} of {}, the continuation diverged",
                evs.len()
            );
            assert_eq!(
                end.snapshot(clock.origin),
                Ok(run.snaps[evs.len()]),
                "{name}: restored at boundary {k}, the final state differs"
            );
        }
    }
}

#[test]
fn a_snapshot_taken_after_the_first_key_carries_negative_offsets_and_continues_exactly() {
    let mut negative = 0usize;
    for (name, evs) in fixtures() {
        let clock = Clock::ahead(Duration::ZERO);
        let epoch = clock.at(2_500 * MS);
        let run = straight(&evs, &clock, epoch);
        for k in 0..=evs.len() {
            let s = run.snaps[k];
            negative += usize::from(s.rate_at.is_some_and(|o| o < 0));
            let (obs, _) = resume(through_bytes(&s, epoch), &evs[k..], &clock);
            assert_eq!(
                first_divergence(&obs, &run.obs[k..]),
                None,
                "{name}: restored at boundary {k} from a later epoch, the continuation diverged"
            );
        }
    }
    assert!(
        negative > 0,
        "fixture: some snapshot holds a key from before its epoch"
    );
}

/// A snapshot restored onto another host's epoch, a week away, continues
/// exactly when the later inputs are rebased with it. Negative control: the
/// same restore with the inputs left on the old clock diverges, so the epoch
/// is what carries time across.
#[test]
fn a_snapshot_restored_onto_another_epoch_continues_exactly() {
    let mut unrebased_diverged = 0usize;
    for (name, evs) in fixtures() {
        let here = Clock::ahead(Duration::ZERO);
        let there =
            Clock::ahead(Duration::from_secs(7 * 86_400) + Duration::from_nanos(123_456_789));
        assert!(there.origin > here.origin, "fixture: the epochs differ");
        let lead = 1_250 * MS;
        let run = straight(&evs, &here, here.at(lead));
        for k in 0..=evs.len() {
            let g = through_bytes(&run.snaps[k], there.at(lead));
            let (obs, end) = resume(g, &evs[k..], &there);
            assert_eq!(
                first_divergence(&obs, &run.obs[k..]),
                None,
                "{name}: restored at boundary {k} onto another epoch, the continuation diverged"
            );
            assert_eq!(end.snapshot(there.at(lead)), Ok(run.snaps[evs.len()]));
            let (stale, _) = resume(g, &evs[k..], &here);
            unrebased_diverged += usize::from(first_divergence(&stale, &run.obs[k..]).is_some());
        }
    }
    assert!(
        unrebased_diverged > 0,
        "negative control: inputs left on the old clock never diverged"
    );
}

#[derive(Clone, Copy, Debug)]
enum Strip {
    RateValue,
    RateAt,
    FpLast,
    FpPrev,
    Latched,
    Stamp,
    Tau,
}

fn strip(g: &mut MomentumGlow, field: Strip) {
    match field {
        Strip::RateValue => g.rate.value = 0.0,
        Strip::RateAt => g.rate.at = None,
        Strip::FpLast => g.fp_last = 0,
        Strip::FpPrev => g.fp_prev = 0,
        Strip::Latched => g.latched = 0.0,
        Strip::Stamp => g.stamp = None,
        Strip::Tau => g.tau = 0.0,
    }
}

/// Negative control for the snapshot's coverage: an engine restored with any
/// one field zeroed (built directly, past `validate`, which would refuse most
/// of them) shows something different on some fixture, so every field the
/// snapshot carries is load-bearing.
#[test]
fn stripping_any_one_field_makes_some_fixture_diverge() {
    let runs: Vec<_> = fixtures()
        .into_iter()
        .map(|(_, evs)| {
            let clock = Clock::ahead(Duration::ZERO);
            let run = straight(&evs, &clock, clock.origin);
            (evs, clock, run)
        })
        .collect();
    for field in [
        Strip::RateValue,
        Strip::RateAt,
        Strip::FpLast,
        Strip::FpPrev,
        Strip::Latched,
        Strip::Stamp,
        Strip::Tau,
    ] {
        let diverged = runs.iter().any(|(evs, clock, run)| {
            (0..evs.len()).any(|k| {
                let mut g = through_bytes(&run.snaps[k], clock.origin);
                strip(&mut g, field);
                let (obs, _) = resume(g, &evs[k..], clock);
                first_divergence(&obs, &run.obs[k..]).is_some()
            })
        });
        assert!(
            diverged,
            "negative control: an engine without {field:?} showed exactly what the straight run did"
        );
    }
}

/// The light-ink memo is a cache: wherever the straight run holds a warm memo,
/// an engine restored with it cold draws every later light frame byte for
/// byte, including after a hue change (a miss) and an ink that differs only
/// in its ignored alpha byte (a hit on another raw key). Negative control: the
/// same engine with a poisoned memo on the current key draws differently, so
/// the comparison sees the memo.
#[test]
fn a_restored_engine_with_a_cold_memo_draws_the_light_page_byte_for_byte() {
    let evs = light_page_burst();
    let clock = Clock::ahead(Duration::ZERO);
    let run = straight(&evs, &clock, clock.origin);
    let (mut warm, mut poison_seen) = (0usize, 0usize);
    for k in 0..evs.len() {
        let original = run.engines[k];
        let Some(memo) = original.light_ink_memo else {
            continue;
        };
        warm += 1;
        let cold = through_bytes(&run.snaps[k], clock.origin);
        assert_eq!(cold.light_ink_memo, None, "restore leaves the memo cold");
        let (from_warm, _) = resume(original, &evs[k..], &clock);
        let (from_cold, _) = resume(cold, &evs[k..], &clock);
        assert_eq!(
            first_divergence(&from_cold, &from_warm),
            None,
            "boundary {k}: a cold memo drew differently from a warm one"
        );
        let poisoned = MomentumGlow {
            light_ink_memo: Some(LightInkMemo {
                ink: memo.ink ^ 0x0020_2020,
                ..memo
            }),
            ..cold
        };
        let (from_poison, _) = resume(poisoned, &evs[k..], &clock);
        poison_seen += usize::from(first_divergence(&from_poison, &from_warm).is_some());
    }
    assert!(warm > 0, "fixture: the light run warms its memo");
    assert!(
        poison_seen > 0,
        "negative control: a poisoned memo never changed a light frame"
    );
}

/// An inert tick appends nothing to either stream, yet changes the state, and
/// the snapshot captures that change: `fp_last` moves to `fp_prev`, and the
/// latch, stamp and `fp_last` clear.
#[test]
fn an_inert_tick_leaves_the_streams_untouched_but_its_state_change_is_captured() {
    let clock = Clock::ahead(Duration::ZERO);
    let mut g = MomentumGlow::default();
    let (geom, cfg) = (padded(), config(true, 0x00FF_FFFF));
    step(&mut g, &clock, &Ev::Key { at: 0, tau: TAU });
    step(
        &mut g,
        &clock,
        &Ev::Tick {
            cur: Some((2, 2)),
            at: 0,
            geom,
            cfg,
        },
    );
    let hot = g.snapshot(clock.origin).expect("snapshots");
    assert!(hot.fp_last != 0 && hot.latched != 0 && hot.stamp.is_some());
    let sentinel = GlowQuad {
        row: 0,
        x: 1,
        y: 1,
        w: 1,
        h: 1,
        color: 7,
        alpha: 0,
        color2: 7,
        alpha2: 0,
    };
    let (mut out, mut under) = (vec![sentinel], vec![sentinel, sentinel]);
    let frame = g.tick(None, clock.at(5 * MS), geom, &cfg, &mut out, &mut under);
    assert_eq!(frame, MomentumGlowFrame::default());
    assert_eq!(
        (out.as_slice(), under.as_slice()),
        (&[sentinel][..], &[sentinel, sentinel][..])
    );
    let after = g.snapshot(clock.origin).expect("snapshots");
    assert_eq!(
        after,
        MomentumGlowSnapshot {
            fp_prev: hot.fp_last,
            fp_last: 0,
            latched: 0,
            stamp: None,
            ..hot
        }
    );
}

/// The rest state is all zeros and absences. Its encoding is the schema
/// followed by 62 zero bytes, and it restores to rest.
#[test]
fn the_rest_state_encodes_as_the_schema_and_zeros_and_restores_to_rest() {
    let epoch = Clock::ahead(Duration::ZERO).origin;
    let rest = MomentumGlow::default().snapshot(epoch).expect("snapshots");
    assert_eq!(
        rest,
        MomentumGlowSnapshot {
            rate_value: 0,
            rate_at: None,
            fp_last: 0,
            fp_prev: 0,
            latched: 0,
            stamp: None,
            tau: 0,
        }
    );
    let mut want = [0u8; MomentumGlowSnapshot::LEN];
    want[0] = 1;
    assert_eq!(rest.encode(), want);
    assert_eq!(rest.validate(), Ok(()));
    let g = MomentumGlow::restore(&rest, epoch).expect("rest restores");
    assert_eq!(g.snapshot(epoch), Ok(rest));
}

/// A hot, stamped state a step could have produced.
fn hot() -> MomentumGlowSnapshot {
    MomentumGlowSnapshot {
        rate_value: 0.7f32.to_bits(),
        rate_at: Some(-5 * MS),
        fp_last: 0x1234_5679,
        fp_prev: 0x1234_5679,
        latched: 0.5f32.to_bits(),
        stamp: Some(3 * MS),
        tau: 1.2f32.to_bits(),
    }
}

/// A cooled state: the rate still stamped, nothing latched, τ dead.
fn cooled() -> MomentumGlowSnapshot {
    MomentumGlowSnapshot {
        rate_value: 0.1f32.to_bits(),
        rate_at: Some(i128::from(i64::MIN)),
        fp_last: 0,
        fp_prev: u64::MAX,
        latched: 0,
        stamp: None,
        tau: f32::MAX.to_bits(),
    }
}

/// The layout table, stated independently of the codec.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    Schema,
    Float(SnapshotField),
    Tag(SnapshotField),
    Payload(SnapshotField),
    Word(SnapshotField),
}

const LAYOUT: [(Slot, usize, usize); 10] = [
    (Slot::Schema, 0, 2),
    (Slot::Float(SnapshotField::RateValue), 2, 4),
    (Slot::Tag(SnapshotField::RateAt), 6, 1),
    (Slot::Payload(SnapshotField::RateAt), 7, 16),
    (Slot::Word(SnapshotField::FpLast), 23, 8),
    (Slot::Word(SnapshotField::FpPrev), 31, 8),
    (Slot::Float(SnapshotField::Latched), 39, 4),
    (Slot::Tag(SnapshotField::Stamp), 43, 1),
    (Slot::Payload(SnapshotField::Stamp), 44, 16),
    (Slot::Float(SnapshotField::Tau), 60, 4),
];

#[test]
fn the_encoding_follows_the_layout_table_byte_for_byte() {
    let s = hot();
    let bytes = s.encode();
    assert_eq!(
        LAYOUT.iter().map(|&(_, _, n)| n).sum::<usize>(),
        MomentumGlowSnapshot::LEN
    );
    for (slot, at, n) in LAYOUT {
        let got = &bytes[at..at + n];
        let want: Vec<u8> = match slot {
            Slot::Schema => MOMENTUM_GLOW_SNAPSHOT_SCHEMA.to_le_bytes().to_vec(),
            Slot::Float(SnapshotField::RateValue) => s.rate_value.to_le_bytes().to_vec(),
            Slot::Float(SnapshotField::Latched) => s.latched.to_le_bytes().to_vec(),
            Slot::Float(_) => s.tau.to_le_bytes().to_vec(),
            Slot::Tag(SnapshotField::RateAt) => vec![u8::from(s.rate_at.is_some())],
            Slot::Tag(_) => vec![u8::from(s.stamp.is_some())],
            Slot::Payload(SnapshotField::RateAt) => s.rate_at.unwrap_or(0).to_le_bytes().to_vec(),
            Slot::Payload(_) => s.stamp.unwrap_or(0).to_le_bytes().to_vec(),
            Slot::Word(SnapshotField::FpLast) => s.fp_last.to_le_bytes().to_vec(),
            Slot::Word(_) => s.fp_prev.to_le_bytes().to_vec(),
        };
        assert_eq!(got, want.as_slice(), "{slot:?} at byte {at}");
    }
}

#[test]
fn encode_then_decode_is_the_identity_at_the_extremes() {
    let extremes = [
        hot(),
        cooled(),
        MomentumGlowSnapshot {
            rate_value: 1.0f32.to_bits(),
            rate_at: Some(i128::MAX),
            fp_last: u64::MAX,
            fp_prev: 1,
            latched: MOMENTUM_GLOW_HOT.to_bits(),
            stamp: Some(i128::MIN),
            tau: 1,
        },
        MomentumGlowSnapshot {
            rate_at: Some(0),
            stamp: Some(0),
            ..hot()
        },
    ];
    for s in extremes {
        assert_eq!(MomentumGlowSnapshot::decode(&s.encode()), Ok(s));
    }
}

#[test]
fn decode_refuses_every_truncation_and_every_extension_by_length() {
    let bytes = hot().encode();
    for n in 0..MomentumGlowSnapshot::LEN {
        assert_eq!(
            MomentumGlowSnapshot::decode(&bytes[..n]),
            Err(SnapshotRefusal::Length {
                expected: MomentumGlowSnapshot::LEN,
                found: n
            })
        );
    }
    for extra in [1usize, 2, 16, 64] {
        let mut long = bytes.to_vec();
        long.resize(MomentumGlowSnapshot::LEN + extra, 0);
        assert_eq!(
            MomentumGlowSnapshot::decode(&long),
            Err(SnapshotRefusal::Length {
                expected: MomentumGlowSnapshot::LEN,
                found: MomentumGlowSnapshot::LEN + extra
            })
        );
    }
}

/// What decoding `s`'s encoding with byte `i` XORed by `mask` must give.
fn expected_after_flip(
    s: &MomentumGlowSnapshot,
    i: usize,
    mask: u8,
) -> Result<MomentumGlowSnapshot, SnapshotRefusal> {
    let mut bytes = s.encode();
    bytes[i] ^= mask;
    let &(slot, at, n) = LAYOUT
        .iter()
        .find(|&&(_, at, n)| (at..at + n).contains(&i))
        .expect("every byte has a slot");
    let field_bytes = &bytes[at..at + n];
    let word = |b: &[u8]| {
        let mut w = [0u8; 16];
        w[..b.len()].copy_from_slice(b);
        u128::from_le_bytes(w)
    };
    match slot {
        Slot::Schema => Err(SnapshotRefusal::Schema {
            found: word(field_bytes) as u16,
        }),
        Slot::Float(field) => {
            let bits = word(field_bytes) as u32;
            if f32::from_bits(bits).is_nan() {
                return Err(SnapshotRefusal::NotANumber { field, bits });
            }
            if bits == 0x8000_0000 {
                return Err(SnapshotRefusal::NegativeZero { field });
            }
            Ok(match field {
                SnapshotField::RateValue => MomentumGlowSnapshot {
                    rate_value: bits,
                    ..*s
                },
                SnapshotField::Latched => MomentumGlowSnapshot {
                    latched: bits,
                    ..*s
                },
                _ => MomentumGlowSnapshot { tau: bits, ..*s },
            })
        }
        Slot::Tag(field) | Slot::Payload(field) => {
            let tag_at = if field == SnapshotField::RateAt {
                6
            } else {
                43
            };
            let tag = bytes[tag_at];
            let payload = word(&bytes[tag_at + 1..tag_at + 17]) as i128;
            let value = match (tag, payload) {
                (0, 0) => None,
                (0, _) => return Err(SnapshotRefusal::NonCanonical { field }),
                (1, p) => Some(p),
                (tag, _) => return Err(SnapshotRefusal::Tag { field, tag }),
            };
            Ok(if field == SnapshotField::RateAt {
                MomentumGlowSnapshot {
                    rate_at: value,
                    ..*s
                }
            } else {
                MomentumGlowSnapshot { stamp: value, ..*s }
            })
        }
        Slot::Word(field) => {
            let value = word(field_bytes) as u64;
            Ok(if field == SnapshotField::FpLast {
                MomentumGlowSnapshot {
                    fp_last: value,
                    ..*s
                }
            } else {
                MomentumGlowSnapshot {
                    fp_prev: value,
                    ..*s
                }
            })
        }
    }
}

/// Every byte under three masks, over a hot, a cooled and the rest state.
#[test]
fn every_byte_flip_is_refused_exactly_or_decodes_to_exactly_the_flipped_field() {
    let epoch = Clock::ahead(Duration::ZERO).origin;
    let rest = MomentumGlow::default().snapshot(epoch).expect("snapshots");
    let (mut refused, mut accepted) = (0usize, 0usize);
    for s in [hot(), cooled(), rest] {
        for i in 0..MomentumGlowSnapshot::LEN {
            for mask in [0x01u8, 0x80, 0xFF] {
                let mut bytes = s.encode();
                bytes[i] ^= mask;
                let got = MomentumGlowSnapshot::decode(&bytes);
                assert_eq!(
                    got,
                    expected_after_flip(&s, i, mask),
                    "byte {i} ^ {mask:#04x}"
                );
                let Ok(d) = got else {
                    refused += 1;
                    continue;
                };
                match MomentumGlow::restore(&d, epoch) {
                    Ok(g) => {
                        accepted += 1;
                        assert_eq!(g.snapshot(epoch), Ok(d), "byte {i} ^ {mask:#04x}");
                    }
                    Err(SnapshotRefusal::Instant { .. }) => refused += 1,
                    Err(e) => {
                        refused += 1;
                        assert_eq!(d.validate(), Err(e), "byte {i} ^ {mask:#04x}");
                    }
                }
            }
        }
    }
    assert!(refused > 0 && accepted > 0, "the sweep reaches both arms");
}

#[test]
fn decode_refuses_nan_and_negative_zero_in_each_float_field() {
    let fields = [
        (SnapshotField::RateValue, 2usize),
        (SnapshotField::Latched, 39),
        (SnapshotField::Tau, 60),
    ];
    for (field, at) in fields {
        for nan in [0x7FC0_0000u32, 0x7F80_0001, 0xFFFF_FFFF, 0xFFC0_0000] {
            let mut bytes = hot().encode();
            bytes[at..at + 4].copy_from_slice(&nan.to_le_bytes());
            assert_eq!(
                MomentumGlowSnapshot::decode(&bytes),
                Err(SnapshotRefusal::NotANumber { field, bits: nan })
            );
        }
        let mut bytes = hot().encode();
        bytes[at..at + 4].copy_from_slice(&(-0.0f32).to_bits().to_le_bytes());
        assert_eq!(
            MomentumGlowSnapshot::decode(&bytes),
            Err(SnapshotRefusal::NegativeZero { field })
        );
    }
}

#[test]
fn decode_refuses_an_unknown_schema_a_bad_tag_and_a_payload_under_none_naming_the_first() {
    for schema in [0u16, 2, 0xFFFF] {
        let mut bytes = hot().encode();
        bytes[..2].copy_from_slice(&schema.to_le_bytes());
        assert_eq!(
            MomentumGlowSnapshot::decode(&bytes),
            Err(SnapshotRefusal::Schema { found: schema })
        );
    }
    let mut bytes = hot().encode();
    bytes[6] = 2;
    assert_eq!(
        MomentumGlowSnapshot::decode(&bytes),
        Err(SnapshotRefusal::Tag {
            field: SnapshotField::RateAt,
            tag: 2
        })
    );
    bytes[43] = 0xFF;
    assert_eq!(
        MomentumGlowSnapshot::decode(&bytes),
        Err(SnapshotRefusal::Tag {
            field: SnapshotField::RateAt,
            tag: 2
        }),
        "the first fault in layout order is named"
    );
    let mut bytes = MomentumGlowSnapshot {
        stamp: None,
        ..hot()
    }
    .encode();
    bytes[59] = 1;
    assert_eq!(
        MomentumGlowSnapshot::decode(&bytes),
        Err(SnapshotRefusal::NonCanonical {
            field: SnapshotField::Stamp
        })
    );
    bytes[0] = 9;
    assert_eq!(
        MomentumGlowSnapshot::decode(&bytes),
        Err(SnapshotRefusal::Schema { found: 9 }),
        "the schema is read before any field"
    );
}

#[test]
fn validate_refuses_each_unreachable_state_with_its_own_variant() {
    let up = |x: f32| f32::from_bits(x.to_bits() + 1).to_bits();
    let down = |x: f32| f32::from_bits(x.to_bits() - 1).to_bits();
    assert_eq!(hot().validate(), Ok(()));
    assert_eq!(cooled().validate(), Ok(()));
    let accepted = [
        MomentumGlowSnapshot {
            rate_value: MOMENTUM_GLOW_KEY_CREDIT.to_bits(),
            ..hot()
        },
        MomentumGlowSnapshot {
            rate_value: 1.0f32.to_bits(),
            latched: 1.0f32.to_bits(),
            ..hot()
        },
        MomentumGlowSnapshot {
            latched: MOMENTUM_GLOW_HOT.to_bits(),
            ..hot()
        },
        MomentumGlowSnapshot {
            tau: MOMENTUM_PROFILE.max_tau_s.to_bits(),
            ..hot()
        },
        MomentumGlowSnapshot { tau: 1, ..hot() },
        MomentumGlowSnapshot {
            rate_value: LIVE_CREDIT_FLOOR.to_bits(),
            ..hot()
        },
        MomentumGlowSnapshot {
            fp_prev: 0,
            tau: 0,
            ..cooled()
        },
        MomentumGlowSnapshot {
            stamp: Some(horizon_ns()),
            rate_at: Some(horizon_ns()),
            ..hot()
        },
    ];
    for s in accepted {
        assert_eq!(s.validate(), Ok(()), "{s:?}");
    }
    let refused = [
        (
            MomentumGlowSnapshot {
                rate_value: down(MOMENTUM_GLOW_KEY_CREDIT),
                ..hot()
            },
            SnapshotRefusal::RateValue {
                bits: down(MOMENTUM_GLOW_KEY_CREDIT),
            },
        ),
        (
            MomentumGlowSnapshot {
                rate_value: up(MOMENTUM_GLOW_KEY_CREDIT),
                ..hot()
            },
            SnapshotRefusal::RateValue {
                bits: up(MOMENTUM_GLOW_KEY_CREDIT),
            },
        ),
        (
            MomentumGlowSnapshot {
                rate_value: down(LIVE_CREDIT_FLOOR),
                ..hot()
            },
            SnapshotRefusal::RateValue {
                bits: down(LIVE_CREDIT_FLOOR),
            },
        ),
        (
            MomentumGlowSnapshot {
                rate_value: up(1.0),
                ..hot()
            },
            SnapshotRefusal::RateValue { bits: up(1.0) },
        ),
        (
            MomentumGlowSnapshot {
                rate_value: f32::NAN.to_bits(),
                ..hot()
            },
            SnapshotRefusal::RateValue {
                bits: f32::NAN.to_bits(),
            },
        ),
        (
            MomentumGlowSnapshot {
                rate_value: (-0.5f32).to_bits(),
                ..hot()
            },
            SnapshotRefusal::RateValue {
                bits: (-0.5f32).to_bits(),
            },
        ),
        (
            MomentumGlowSnapshot {
                rate_at: None,
                ..hot()
            },
            SnapshotRefusal::RateStamp {
                value_bits: 0.7f32.to_bits(),
                stamped: false,
            },
        ),
        (
            MomentumGlowSnapshot {
                rate_value: 0,
                ..cooled()
            },
            SnapshotRefusal::RateStamp {
                value_bits: 0,
                stamped: true,
            },
        ),
        (
            MomentumGlowSnapshot {
                latched: down(MOMENTUM_GLOW_HOT),
                ..hot()
            },
            SnapshotRefusal::Latched {
                bits: down(MOMENTUM_GLOW_HOT),
            },
        ),
        (
            MomentumGlowSnapshot {
                latched: up(1.0),
                ..hot()
            },
            SnapshotRefusal::Latched { bits: up(1.0) },
        ),
        (
            MomentumGlowSnapshot {
                latched: f32::INFINITY.to_bits(),
                ..hot()
            },
            SnapshotRefusal::Latched {
                bits: f32::INFINITY.to_bits(),
            },
        ),
        (
            MomentumGlowSnapshot {
                latched: 0,
                ..hot()
            },
            SnapshotRefusal::Latch {
                latched_bits: 0,
                stamped: true,
                fp_last: 0x1234_5679,
            },
        ),
        (
            MomentumGlowSnapshot {
                stamp: None,
                ..hot()
            },
            SnapshotRefusal::Latch {
                latched_bits: 0.5f32.to_bits(),
                stamped: false,
                fp_last: 0x1234_5679,
            },
        ),
        (
            MomentumGlowSnapshot {
                fp_last: 0,
                ..hot()
            },
            SnapshotRefusal::Latch {
                latched_bits: 0.5f32.to_bits(),
                stamped: true,
                fp_last: 0,
            },
        ),
        (
            MomentumGlowSnapshot {
                fp_last: 2,
                ..hot()
            },
            SnapshotRefusal::Fingerprint {
                field: SnapshotField::FpLast,
                value: 2,
            },
        ),
        (
            MomentumGlowSnapshot {
                fp_prev: 1 << 63,
                ..cooled()
            },
            SnapshotRefusal::Fingerprint {
                field: SnapshotField::FpPrev,
                value: 1 << 63,
            },
        ),
        (
            MomentumGlowSnapshot {
                rate_value: 0,
                rate_at: None,
                ..hot()
            },
            SnapshotRefusal::HotWithoutRate {
                latched_bits: 0.5f32.to_bits(),
            },
        ),
        (
            MomentumGlowSnapshot { tau: 0, ..hot() },
            SnapshotRefusal::Tau { bits: 0, hot: true },
        ),
        (
            MomentumGlowSnapshot {
                tau: up(MOMENTUM_PROFILE.max_tau_s),
                ..hot()
            },
            SnapshotRefusal::Tau {
                bits: up(MOMENTUM_PROFILE.max_tau_s),
                hot: true,
            },
        ),
        (
            MomentumGlowSnapshot {
                tau: f32::NAN.to_bits(),
                ..hot()
            },
            SnapshotRefusal::Tau {
                bits: f32::NAN.to_bits(),
                hot: true,
            },
        ),
        (
            MomentumGlowSnapshot {
                tau: f32::INFINITY.to_bits(),
                ..cooled()
            },
            SnapshotRefusal::Tau {
                bits: f32::INFINITY.to_bits(),
                hot: false,
            },
        ),
        (
            MomentumGlowSnapshot {
                tau: (-0.0f32).to_bits(),
                ..cooled()
            },
            SnapshotRefusal::Tau {
                bits: (-0.0f32).to_bits(),
                hot: false,
            },
        ),
        (
            MomentumGlowSnapshot {
                tau: (-1.0f32).to_bits(),
                ..cooled()
            },
            SnapshotRefusal::Tau {
                bits: (-1.0f32).to_bits(),
                hot: false,
            },
        ),
        (
            MomentumGlowSnapshot {
                fp_prev: 1,
                ..rest()
            },
            SnapshotRefusal::History {
                rate_bits: 0,
                fp_prev: 1,
                tau: 0,
            },
        ),
        (
            MomentumGlowSnapshot {
                tau: TAU.to_bits(),
                ..rest()
            },
            SnapshotRefusal::History {
                rate_bits: 0,
                fp_prev: 0,
                tau: TAU.to_bits(),
            },
        ),
        (
            MomentumGlowSnapshot { tau: 0, ..cooled() },
            SnapshotRefusal::History {
                rate_bits: 0.1f32.to_bits(),
                fp_prev: u64::MAX,
                tau: 0,
            },
        ),
        (
            MomentumGlowSnapshot {
                stamp: Some(horizon_ns() + 1),
                ..hot()
            },
            SnapshotRefusal::Horizon {
                field: SnapshotField::Stamp,
                offset: horizon_ns() + 1,
            },
        ),
        (
            MomentumGlowSnapshot {
                rate_at: Some(horizon_ns() + 1),
                ..hot()
            },
            SnapshotRefusal::Horizon {
                field: SnapshotField::RateAt,
                offset: horizon_ns() + 1,
            },
        ),
        (
            MomentumGlowSnapshot {
                rate_at: Some(i128::MAX),
                stamp: Some(i128::MAX),
                ..hot()
            },
            SnapshotRefusal::Horizon {
                field: SnapshotField::RateAt,
                offset: i128::MAX,
            },
        ),
    ];
    let epoch = Clock::ahead(Duration::ZERO).origin;
    for (s, want) in refused {
        assert_eq!(s.validate(), Err(want), "{s:?}");
        assert_eq!(
            MomentumGlow::restore(&s, epoch).map(|g| g.snapshot(epoch)),
            Err(want),
            "restore validates first: {s:?}"
        );
    }
}

/// The rest state.
fn rest() -> MomentumGlowSnapshot {
    MomentumGlow::default()
        .snapshot(Clock::ahead(Duration::ZERO).origin)
        .expect("snapshots")
}

/// The hosted horizon in nanoseconds.
fn horizon_ns() -> i128 {
    i128::try_from(MOMENTUM_PROFILE.horizon.as_nanos()).expect("the horizon fits i128 ns")
}

/// A negative offset is never past the horizon, so only the host's `Instant`
/// range bounds it: one beyond it is refused at restore, and one inside it,
/// decades before the epoch, restores and schedules without a panic.
#[test]
fn restore_refuses_a_negative_offset_outside_the_instant_range_and_accepts_one_inside_it() {
    let epoch = Clock::ahead(Duration::ZERO).origin;
    let far = i128::from(u64::MAX) * 1_000_000_000;
    let cases = [
        (
            MomentumGlowSnapshot {
                rate_at: Some(i128::MIN),
                ..hot()
            },
            SnapshotField::RateAt,
            i128::MIN,
        ),
        (
            MomentumGlowSnapshot {
                stamp: Some(-far),
                ..hot()
            },
            SnapshotField::Stamp,
            -far,
        ),
    ];
    for (s, field, offset) in cases {
        assert_eq!(
            s.validate(),
            Ok(()),
            "fixture: only the instant is out of range"
        );
        assert_eq!(
            MomentumGlow::restore(&s, epoch).map(|g| g.snapshot(epoch)),
            Err(SnapshotRefusal::Instant { field, offset })
        );
    }
    let decades = 1_000_000_000 * 1_000_000_000;
    let old = MomentumGlowSnapshot {
        rate_at: Some(-decades - 5 * MS),
        stamp: Some(-decades),
        ..hot()
    };
    let g = MomentumGlow::restore(&old, epoch).expect("an old stamp restores");
    assert_eq!(g.snapshot(epoch), Ok(old));
    let stamp = Clock { origin: epoch }.at(-decades);
    assert!(g.cold_at().is_some_and(|t| t > stamp));
    assert!(g.next_change_deadline(stamp, LANE).is_some());
}

/// A stamp exactly at the horizon restores, and its schedule forms; one past
/// it is refused before the epoch is consulted (the validate table holds the
/// exact refusals).
#[test]
fn a_stamp_at_the_horizon_restores_and_schedules_and_one_past_it_is_refused() {
    let clock = Clock::ahead(Duration::ZERO);
    let at = MomentumGlowSnapshot {
        stamp: Some(horizon_ns()),
        rate_at: Some(horizon_ns()),
        ..hot()
    };
    let g = MomentumGlow::restore(&at, clock.origin).expect("the horizon itself restores");
    let stamp = clock.at(horizon_ns());
    assert!(g.settling());
    let cold = g.cold_at().expect("latched");
    assert!(cold > stamp);
    let wake = g
        .next_change_deadline(stamp, LANE)
        .expect("settling names a wake");
    assert!(wake > stamp && wake <= cold + SETTLE_MARGIN);
    let past = MomentumGlowSnapshot {
        stamp: Some(horizon_ns() + 1),
        ..at
    };
    assert_eq!(
        MomentumGlow::restore(&past, clock.origin).map(|g| g.snapshot(clock.origin)),
        Err(SnapshotRefusal::Horizon {
            field: SnapshotField::Stamp,
            offset: horizon_ns() + 1
        })
    );
}

/// A 120 Hz lane.
const LANE: Duration = Duration::from_micros(8_333);

/// The latest whole-second instant `checked_add` forms from `base`, found by bisection.
fn instant_ceiling(base: Instant) -> Instant {
    let (mut lo, mut hi) = (0u64, u64::MAX);
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if base.checked_add(Duration::from_secs(mid)).is_some() {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    base.checked_add(Duration::from_secs(lo))
        .expect("the bisection's floor is formable")
}

/// Near the platform's `Instant` ceiling the epoch itself is representable,
/// but a stamp whose settle schedule (`τ·ln 255 + SETTLE_MARGIN`, 333 s at the
/// profile's longest τ) would cross the ceiling is refused at restore.
#[test]
fn near_the_instant_ceiling_a_stamp_whose_schedule_cannot_form_is_refused_and_any_other_schedules()
{
    let ceiling = instant_ceiling(Instant::now());
    let room = |secs: u64| {
        ceiling
            .checked_sub(Duration::from_secs(secs))
            .expect("the ceiling has room below it")
    };
    let slow = MOMENTUM_PROFILE.max_tau_s;
    let cases = [
        (100u64, slow, false),
        (100, TAU, true),
        (400, slow, true),
        (5, TAU, false),
        (10, TAU, true),
    ];
    for (secs, tau, fits) in cases {
        let epoch = room(secs);
        let s = MomentumGlowSnapshot {
            tau: tau.to_bits(),
            rate_at: Some(0),
            stamp: Some(0),
            ..hot()
        };
        let restored = MomentumGlow::restore(&s, epoch);
        if !fits {
            assert_eq!(
                restored.map(|g| g.snapshot(epoch)),
                Err(SnapshotRefusal::Instant {
                    field: SnapshotField::Stamp,
                    offset: 0
                }),
                "{secs} s of room, tau {tau}"
            );
            continue;
        }
        let g = restored.unwrap_or_else(|e| panic!("{secs} s of room, tau {tau}: {e}"));
        let cold = g.cold_at().expect("latched");
        assert!(cold > epoch, "{secs} s of room, tau {tau}");
        let wake = g
            .next_change_deadline(epoch, LANE)
            .expect("settling names a wake");
        assert!(
            wake > epoch && wake <= cold + SETTLE_MARGIN,
            "{secs} s of room, tau {tau}"
        );
        assert!(g.value(epoch, tau) > 0.0);
    }
}

/// The rate's floor is what `on_key` stores over the least value that has
/// not snapped (`SNAP_ZERO`, read with no time elapsed), and a key over a
/// rate that reads 0 stores exactly the credit. Both validate; the bit just
/// above the credit and just below the floor do not (the validate table).
#[test]
fn the_rate_floor_is_what_a_key_stores_over_the_least_live_value() {
    let clock = Clock::ahead(Duration::ZERO);
    let now = clock.at(0);
    let mut live = MomentumGlow {
        rate: TypingRate {
            value: SNAP_ZERO,
            at: Some(now),
        },
        ..MomentumGlow::default()
    };
    assert_eq!(live.value(now, TAU), SNAP_ZERO);
    live.on_key(now, TAU);
    let s = live.snapshot(clock.origin).expect("snapshots");
    assert_eq!(s.rate_value, LIVE_CREDIT_FLOOR.to_bits());
    assert_eq!(s.validate(), Ok(()));
    let mut cold = MomentumGlow::default();
    cold.on_key(now, TAU);
    let s = cold.snapshot(clock.origin).expect("snapshots");
    assert_eq!(s.rate_value, MOMENTUM_GLOW_KEY_CREDIT.to_bits());
    assert_eq!(s.validate(), Ok(()));
    const { assert!(MOMENTUM_GLOW_KEY_CREDIT < LIVE_CREDIT_FLOOR) };
}

/// A hand-built τ outside the profile is refused at restore even though the
/// engine computes with it, because no hosted tick latches it. The longest
/// admitted τ restores and continues like the straight run (the "resets and
/// inert arms" fixture latches it).
#[test]
fn a_latched_tau_past_the_profile_is_refused_and_the_profiles_own_is_restored() {
    let epoch = Clock::ahead(Duration::ZERO).origin;
    let at_limit = MomentumGlowSnapshot {
        tau: MOMENTUM_PROFILE.max_tau_s.to_bits(),
        ..hot()
    };
    assert!(MomentumGlow::restore(&at_limit, epoch).is_ok());
    let past = MomentumGlowSnapshot {
        tau: 61.0f32.to_bits(),
        ..hot()
    };
    assert_eq!(
        MomentumGlow::restore(&past, epoch).map(|g| g.snapshot(epoch)),
        Err(SnapshotRefusal::Tau {
            bits: 61.0f32.to_bits(),
            hot: true
        })
    );
    let cfg = MomentumGlowConfig {
        tau_s: MOMENTUM_PROFILE.max_tau_s,
        ..config(true, 0x00FF_FFFF)
    };
    let clock = Clock { origin: epoch };
    let mut g = MomentumGlow::default();
    step(&mut g, &clock, &Ev::Key { at: 0, tau: TAU });
    step(
        &mut g,
        &clock,
        &Ev::Tick {
            cur: Some((1, 1)),
            at: 0,
            geom: padded(),
            cfg,
        },
    );
    let s = g.snapshot(epoch).expect("snapshots");
    assert_eq!(s.tau, MOMENTUM_PROFILE.max_tau_s.to_bits());
    assert_eq!(
        MomentumGlow::restore(&s, epoch).map(|r| r.snapshot(epoch)),
        Ok(Ok(s))
    );
}

/// splitmix64, seeded.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[usize::try_from(self.below(xs.len() as u64)).expect("an index fits usize")]
    }

    /// Uniform in `[0, 1)`.
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// A τ a tick latches.
fn random_tau(r: &mut Rng) -> f32 {
    match r.below(7) {
        0 => TAU,
        1 => 0.8,
        2 => MOMENTUM_PROFILE.max_tau_s,
        3 => 0.05,
        4 => f32::from_bits(1),
        5 => 1e-3,
        _ => (r.unit() * MOMENTUM_PROFILE.max_tau_s).max(f32::MIN_POSITIVE),
    }
}

/// A key's τ, which the bare engine takes unadmitted.
fn random_key_tau(r: &mut Rng) -> f32 {
    match r.below(12) {
        0 => f32::NAN,
        1 => 0.0,
        2 => -1.0,
        3 => f32::INFINITY,
        4 => 90.0,
        _ => random_tau(r),
    }
}

/// The time to the next input.
fn random_gap(r: &mut Rng) -> i128 {
    let n = |r: &mut Rng, k: u64| i128::from(r.below(k));
    match r.below(8) {
        0 | 1 => 0,
        2 | 3 => (n(r, 40) + 1) * MS,
        4 => n(r, 800) * MS,
        5 => 2_700 * MS + n(r, 1_000) * MS,
        6 => (n(r, 60) + 1) * 1_000 * MS,
        _ => (n(r, 48) + 1) * 3_600_000 * MS,
    }
}

/// A tick on a dark or a light page, admitted under the profile, with each
/// inert arm (disabled, zero intensity, no cursor, outside the grid) drawn
/// now and then.
fn random_tick(r: &mut Rng, at: i128) -> Ev {
    let dark = r.below(2) == 0;
    let geom = if r.below(2) == 0 { plain() } else { padded() };
    let hue = r.pick(&[0x0050_FA7Bu32, 0x0009_69DA, 0x00CF_222E, 0x00A0_A0A0]);
    let cfg = MomentumGlowConfig {
        enabled: r.below(10) != 0,
        intensity: match r.below(6) {
            0 => 0.0,
            1 => 1.0,
            2 => 0.25,
            _ => r.unit(),
        },
        tau_s: random_tau(r),
        theme_fg: if r.below(4) == 0 {
            0xFF24_292F
        } else {
            config(dark, hue).theme_fg
        },
        glyph_span: (!dark && r.below(3) != 0).then(|| {
            let end = i32::try_from(r.below(30)).expect("fits");
            (end / 3, end)
        }),
        block: r.below(3) != 0,
        ..config(dark, hue)
    };
    let cur = match r.below(10) {
        0 => None,
        1 => Some((u16::try_from(geom.rows).expect("fits"), 0)),
        _ => Some((
            u16::try_from(r.below(geom.rows as u64)).expect("fits"),
            u16::try_from(r.below(geom.cols as u64)).expect("fits"),
        )),
    };
    Ev::Tick { cur, at, geom, cfg }
}

/// What a random stream reached, so the sweep can show it was not vacuous.
#[derive(Debug, Default)]
struct Reached {
    hot: usize,
    settling: usize,
    light_hot: usize,
    exactly_credit: usize,
    near_floor: usize,
    rest_after_a_hot_reset: usize,
    cold_with_an_old_stamp: usize,
    subnormal_tau_latched: usize,
    longest_tau_latched: usize,
    negative_offsets: usize,
}

/// Every state random engine streams reach is accepted by `validate`, and
/// restores to itself through its bytes, snapshotted at four epochs: the
/// stream's start, half a day in, the horizon (every offset ≤ 0) and 400 days
/// later (old stamps). The streams mix keys (some with τ the bare engine
/// takes unadmitted), resets, dark and light ticks under τ from subnormal to
/// the profile's longest, each inert arm, a tick repeated (settling), schedule
/// queries, repeated instants, gaps from a frame to two days, and now and then a time that goes
/// back (a bare engine fed by two clocks). Times stay in `[0, horizon]`.
#[test]
fn validate_accepts_every_state_random_engine_streams_reach_at_every_epoch() {
    let clock = Clock::ahead(Duration::ZERO);
    let horizon = horizon_ns();
    let epochs = [
        clock.origin,
        clock.at(12 * 3_600_000 * MS),
        clock.at(horizon),
        clock.origin + Duration::from_secs(400 * 86_400),
    ];
    let lane = Duration::from_micros(16_667);
    let mut reached = Reached::default();
    for seed in 0..48u64 {
        let mut r = Rng(seed.wrapping_mul(0x2545_F491_4F6C_DD1D) ^ 0x6A09_E667);
        let mut g = MomentumGlow::default();
        let (mut at, mut hot_since_reset, mut reset_after_hot) = (0i128, false, false);
        let mut last_tick = None;
        for k in 0..300 {
            at = (at + random_gap(&mut r)).min(horizon);
            if r.below(40) == 0 {
                at -= i128::from(r.below(u64::try_from(at).expect("non-negative") + 1));
            }
            let ev = match (r.below(20), last_tick) {
                (0, _) => Ev::Reset,
                (1..=7, _) => Ev::Key {
                    at,
                    tau: random_key_tau(&mut r),
                },
                (8..=10, Some(Ev::Tick { cur, geom, cfg, .. })) => Ev::Tick { cur, at, geom, cfg },
                (8..=16, _) => random_tick(&mut r, at),
                _ => Ev::Query { at, lane },
            };
            if matches!(ev, Ev::Tick { .. }) {
                last_tick = Some(ev);
            }
            let obs = step(&mut g, &clock, &ev);
            match (&ev, &obs) {
                (Ev::Reset, _) => {
                    reset_after_hot = hot_since_reset;
                    hot_since_reset = false;
                }
                (Ev::Tick { cfg, .. }, Obs::Frame { frame, .. }) if frame.hot => {
                    hot_since_reset = true;
                    reached.light_hot += usize::from(!cfg.dark_theme);
                }
                _ => {}
            }
            for (e, &epoch) in epochs.iter().enumerate() {
                let s = g.snapshot(epoch).expect("a live engine snapshots");
                assert_eq!(
                    s.validate(),
                    Ok(()),
                    "seed {seed}, input {k}, epoch {e}: {s:?}"
                );
                let back = MomentumGlowSnapshot::decode(&s.encode())
                    .and_then(|d| MomentumGlow::restore(&d, epoch))
                    .unwrap_or_else(|f| panic!("seed {seed}, input {k}, epoch {e}: {f}"));
                assert_eq!(
                    back.snapshot(epoch),
                    Ok(s),
                    "seed {seed}, input {k}, epoch {e}"
                );
                reached.negative_offsets +=
                    usize::from(s.rate_at.is_some_and(|o| o < 0) || s.stamp.is_some_and(|o| o < 0));
            }
            let s = g.snapshot(clock.origin).expect("snapshots");
            let latched = s.latched != 0;
            reached.hot += usize::from(latched);
            reached.settling += usize::from(g.settling());
            reached.exactly_credit +=
                usize::from(s.rate_value == MOMENTUM_GLOW_KEY_CREDIT.to_bits());
            reached.near_floor += usize::from(
                (LIVE_CREDIT_FLOOR.to_bits()..=0.11f32.to_bits()).contains(&s.rate_value),
            );
            reached.rest_after_a_hot_reset += usize::from(s.rate_value == 0 && reset_after_hot);
            reached.cold_with_an_old_stamp +=
                usize::from(s.rate_value != 0 && g.value(clock.at(at), TAU) == 0.0);
            reached.subnormal_tau_latched += usize::from(latched && s.tau == 1);
            reached.longest_tau_latched +=
                usize::from(latched && s.tau == MOMENTUM_PROFILE.max_tau_s.to_bits());
        }
    }
    for (what, n) in [
        ("hot", reached.hot),
        ("settling", reached.settling),
        ("light page hot", reached.light_hot),
        ("rate exactly the credit", reached.exactly_credit),
        ("rate near the live floor", reached.near_floor),
        (
            "rest after a reset of a hot glow",
            reached.rest_after_a_hot_reset,
        ),
        ("cold with an old stamp", reached.cold_with_an_old_stamp),
        ("subnormal tau latched", reached.subnormal_tau_latched),
        ("longest tau latched", reached.longest_tau_latched),
        ("negative offsets", reached.negative_offsets),
    ] {
        assert!(n > 0, "the sweep never reached: {what} ({reached:?})");
    }
}
