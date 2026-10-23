// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The task ABI's encodings and artifacts.

use std::time::Duration;

use aterm_aos::task::{PausedClass, Schema, state_digest};
use aterm_aos::wire::{Dec, Enc, WireFault};
use aterm_aos::{ArtifactKind, TaskIdentity};
use aterm_render::GlowQuad;
use aterm_time::Instant;

use super::test_fixtures::{BUILD, Call, cfg, geom, headed_geom, play, random_stream};
use super::{
    CONTRACT_NAME, ConfigRecord, GeomRecord, MomentumCode, MomentumEvent, MomentumHost,
    MomentumObservation, MomentumSchema, MomentumState, NUMERIC_PROFILE, NoStrandedLight,
    PrebuiltMomentum, SCHEMA_NAME, TASK_NAME, momentum_identity, profile_artifact, schema_artifact,
    task_artifact,
};
use crate::cursor_momentum::{
    AdmitRefusal, GeometryFault, MOMENTUM_GLOW_TAU_S, MomentumGlow, MomentumGlowSnapshot,
};

fn encode_input(schema: &MomentumSchema, ev: &MomentumEvent) -> Vec<u8> {
    let mut e = Enc::new();
    schema.encode_input(ev, &mut e);
    e.finish()
}

fn decode_input(schema: &MomentumSchema, bytes: &[u8]) -> Result<MomentumEvent, WireFault> {
    let mut d = Dec::new(bytes);
    let ev = schema.decode_input(&mut d)?;
    d.finish()?;
    Ok(ev)
}

fn encode_state(schema: &MomentumSchema, s: &MomentumState) -> Vec<u8> {
    let mut e = Enc::new();
    schema.encode_state(s, &mut e);
    e.finish()
}

fn decode_state(schema: &MomentumSchema, bytes: &[u8]) -> Result<MomentumState, WireFault> {
    let mut d = Dec::new(bytes);
    let s = schema.decode_state(&mut d)?;
    d.finish()?;
    Ok(s)
}

/// Why `bytes` is not a state, if it is not.
fn state_fault(schema: &MomentumSchema, bytes: &[u8]) -> Option<WireFault> {
    decode_state(schema, bytes).err()
}

fn encode_observation(schema: &MomentumSchema, o: &MomentumObservation) -> Vec<u8> {
    let mut e = Enc::new();
    schema.encode_observation(o, &mut e);
    e.finish()
}

/// A tick at the extremes of every field.
fn extreme_tick() -> MomentumEvent {
    MomentumEvent::Tick {
        cur: Some((u16::MAX, u16::MAX)),
        at: i128::MIN,
        geom: GeomRecord {
            cw: usize::MAX,
            ch: 0,
            rows: usize::MAX,
            cols: 1,
            origin_x: u16::MAX,
            origin_y: 0,
            win_w: u16::MAX,
            win_h: 1,
            head: u16::MAX,
        },
        cfg: ConfigRecord {
            enabled: true,
            intensity: f32::NAN.to_bits(),
            tau_s: f32::NEG_INFINITY.to_bits(),
            radius_cells: (-0.0f32).to_bits(),
            base: u32::MAX,
            dark_theme: false,
            hue: 0,
            theme_fg: u32::MAX,
            theme_bg: 1,
            block: true,
            glyph_span: Some((i32::MIN, i32::MAX)),
        },
    }
}

/// One input of every kind, the shipped host's and the extremes of every field.
fn inputs() -> Vec<MomentumEvent> {
    let c = cfg(true);
    vec![
        MomentumEvent::Key {
            at: 0,
            tau_bits: MOMENTUM_GLOW_TAU_S.to_bits(),
        },
        MomentumEvent::Key {
            at: i128::MAX,
            tau_bits: f32::NAN.to_bits(),
        },
        MomentumEvent::Key {
            at: -1,
            tau_bits: u32::MAX,
        },
        MomentumEvent::Reset,
        MomentumEvent::Tick {
            cur: None,
            at: 16_666_667,
            geom: GeomRecord::from(&geom()),
            cfg: ConfigRecord::from(&c),
        },
        MomentumEvent::Tick {
            cur: Some((4, 10)),
            at: 1,
            geom: GeomRecord::from(&headed_geom()),
            cfg: ConfigRecord::from(&cfg(false)),
        },
        extreme_tick(),
        MomentumEvent::Query {
            at: 5,
            frame_interval: Duration::from_nanos(16_666_667),
        },
        MomentumEvent::Query {
            at: i128::MIN,
            frame_interval: Duration::MAX,
        },
        MomentumEvent::Query {
            at: i128::MAX,
            frame_interval: Duration::ZERO,
        },
    ]
}

/// Every input, the shipped host's and the extremes of every field (offsets at `i128`'s
/// ends, `usize::MAX` geometry, NaN and negative-zero bits, the widest glyph span,
/// `Duration::MAX`), decodes to itself and re-encodes to the same bytes; the geometry and
/// config records carry every field bit for bit.
#[test]
fn every_input_round_trips_through_its_encoding_including_extremes() {
    let schema = MomentumSchema::new(Instant::now());
    for ev in inputs() {
        let bytes = encode_input(&schema, &ev);
        let back = decode_input(&schema, &bytes).expect("an encoded input decodes");
        assert_eq!(back, ev);
        assert_eq!(encode_input(&schema, &back), bytes);
    }
    for g in [geom(), headed_geom()] {
        let r = GeomRecord::from(&g);
        assert_eq!(GeomRecord::from(&r.geom()), r);
    }
    for dark in [true, false] {
        let mut c = cfg(dark);
        c.glyph_span = Some((-3, 7));
        c.intensity = f32::NAN;
        let r = ConfigRecord::from(&c);
        assert_eq!(ConfigRecord::from(&r.config()), r);
        assert_eq!(r.config().intensity.to_bits(), f32::NAN.to_bits());
    }
}

/// A tag no input encoding writes is `BadTag` on `event.tag`.
#[test]
fn an_unknown_input_tag_is_refused_naming_it() {
    let schema = MomentumSchema::new(Instant::now());
    for tag in [4u8, 5, 0x7F, 0xFF] {
        assert_eq!(
            decode_input(&schema, &[tag]),
            Err(WireFault::BadTag {
                field: "event.tag",
                tag
            })
        );
    }
}

/// Every proper prefix of every input's encoding is `Truncated`, and one byte past its end
/// is `Trailing { extra: 1 }`.
#[test]
fn every_truncation_of_an_input_is_refused_and_so_is_a_trailing_byte() {
    let schema = MomentumSchema::new(Instant::now());
    for ev in inputs() {
        let bytes = encode_input(&schema, &ev);
        for n in 0..bytes.len() {
            assert!(
                matches!(
                    decode_input(&schema, &bytes[..n]),
                    Err(WireFault::Truncated { .. })
                ),
                "{ev:?} cut at {n}: {:?}",
                decode_input(&schema, &bytes[..n])
            );
        }
        let mut long = bytes.clone();
        long.push(0);
        assert_eq!(
            decode_input(&schema, &long),
            Err(WireFault::Trailing { extra: 1 })
        );
    }
}

/// A change to one config field.
type Flip = fn(&mut ConfigRecord);

/// The byte where two encodings differ, when they differ in exactly one byte.
fn sole_difference(a: &[u8], b: &[u8]) -> usize {
    assert_eq!(a.len(), b.len());
    let at: Vec<usize> = (0..a.len()).filter(|&i| a[i] != b[i]).collect();
    assert_eq!(at.len(), 1, "fixture: the encodings differ in one byte");
    at[0]
}

/// A bool byte of 2 is `BadTag` on its field (`cfg.enabled`, `cfg.dark_theme`,
/// `cfg.block`), and an option tag of 2 is `BadTag` on its field (`event.tick.cur`,
/// `cfg.glyph_span`).
#[test]
fn a_bool_or_option_byte_no_encoding_writes_is_refused_naming_its_field() {
    let schema = MomentumSchema::new(Instant::now());
    let tick = |f: Flip, cur: Option<(u16, u16)>| {
        let mut c = ConfigRecord::from(&cfg(true));
        f(&mut c);
        encode_input(
            &schema,
            &MomentumEvent::Tick {
                cur,
                at: 7,
                geom: GeomRecord::from(&geom()),
                cfg: c,
            },
        )
    };
    let bools: [(&str, Flip); 3] = [
        ("cfg.enabled", |c| c.enabled = false),
        ("cfg.dark_theme", |c| c.dark_theme = false),
        ("cfg.block", |c| c.block = false),
    ];
    let base = tick(|_| {}, None);
    for (field, flip) in bools {
        let at = sole_difference(&base, &tick(flip, None));
        let mut bad = base.clone();
        bad[at] = 2;
        assert_eq!(
            decode_input(&schema, &bad),
            Err(WireFault::BadTag { field, tag: 2 })
        );
    }
    let mut bad = base.clone();
    bad[1] = 2;
    assert_eq!(
        decode_input(&schema, &bad),
        Err(WireFault::BadTag {
            field: "event.tick.cur",
            tag: 2
        })
    );
    let spanned = tick(|c| c.glyph_span = Some((1, 2)), None);
    let mut bad = spanned.clone();
    let span_tag = spanned.len() - 9;
    assert_eq!(spanned[span_tag], 1, "fixture: the glyph span's tag");
    bad[span_tag] = 2;
    assert_eq!(
        decode_input(&schema, &bad[..span_tag + 1]),
        Err(WireFault::BadTag {
            field: "cfg.glyph_span",
            tag: 2
        })
    );
}

/// A frame interval whose nanoseconds reach a second is `NonCanonical` on
/// `event.query.frame_interval`.
#[test]
fn a_frame_interval_with_a_second_of_nanoseconds_is_refused() {
    let schema = MomentumSchema::new(Instant::now());
    let mut bytes = encode_input(
        &schema,
        &MomentumEvent::Query {
            at: 0,
            frame_interval: Duration::from_nanos(999_999_999),
        },
    );
    let nanos = bytes.len() - 4;
    bytes[nanos..].copy_from_slice(&1_000_000_000u32.to_le_bytes());
    assert_eq!(
        decode_input(&schema, &bytes),
        Err(WireFault::NonCanonical {
            field: "event.query.frame_interval"
        })
    );
}

/// Every state a host reaches on random streams (inside and outside the profile, unattached
/// engines driven exactly as a host drives them) encodes, decodes and re-encodes to the same
/// bytes, so its digest survives a checkpoint; the clock round-trips at its extremes.
#[test]
fn every_reachable_state_round_trips_byte_for_byte() {
    let t0 = Instant::now();
    let schema = MomentumSchema::new(t0);
    for seed in 1..=8u64 {
        for in_profile in [true, false] {
            let calls = random_stream(seed, 300, in_profile);
            for k in (0..=calls.len()).step_by(7) {
                let mut glow = MomentumGlow::default();
                let _ = play(&mut glow, t0, &calls[..k]);
                for clock in [None, Some(0), Some(i128::MIN), Some(i128::MAX)] {
                    let s = MomentumState { glow, clock };
                    let bytes = encode_state(&schema, &s);
                    assert_eq!(
                        bytes.len(),
                        MomentumGlowSnapshot::LEN + 1 + 16 * usize::from(clock.is_some())
                    );
                    let back = decode_state(&schema, &bytes)
                        .unwrap_or_else(|f| panic!("seed {seed} at {k}: {f}"));
                    assert_eq!(back.clock(), clock);
                    assert_eq!(encode_state(&schema, &back), bytes);
                    assert_eq!(state_digest(&schema, &back), state_digest(&schema, &s));
                }
            }
        }
    }
}

/// A state whose bytes no encoder writes is refused naming the field: a short snapshot is
/// `Truncated` on `state.snapshot`; another snapshot schema is `NonCanonical` on
/// `state.snapshot.schema`; a bad offset tag is `BadTag` on `rate_at`; a canonical but
/// unreachable snapshot (a stamped rate of zero) is `NonCanonical` on `rate_at`; a clock
/// tag of 2 is `BadTag` on `state.clock`; and a trailing byte is `Trailing`.
#[test]
fn a_state_that_is_not_canonical_or_not_reachable_is_refused_naming_its_field() {
    let t0 = Instant::now();
    let schema = MomentumSchema::new(t0);
    let mut glow = MomentumGlow::default();
    glow.on_key(t0, MOMENTUM_GLOW_TAU_S);
    let good = encode_state(
        &schema,
        &MomentumState {
            glow,
            clock: Some(3),
        },
    );
    assert_eq!(state_fault(&schema, &good), None);
    assert_eq!(
        state_fault(&schema, &good[..40]),
        Some(WireFault::Truncated {
            field: "state.snapshot",
            need: MomentumGlowSnapshot::LEN,
            have: 40
        })
    );
    let mut other_schema = good.clone();
    other_schema[..2].copy_from_slice(&2u16.to_le_bytes());
    assert_eq!(
        state_fault(&schema, &other_schema),
        Some(WireFault::NonCanonical {
            field: "state.snapshot.schema"
        })
    );
    let mut bad_tag = good.clone();
    assert_eq!(bad_tag[6], 1, "fixture: the rate is stamped");
    bad_tag[6] = 2;
    assert_eq!(
        state_fault(&schema, &bad_tag),
        Some(WireFault::BadTag {
            field: "rate_at",
            tag: 2
        })
    );
    let mut unreachable = glow.snapshot(t0).expect("a snapshot");
    unreachable.rate_value = 0;
    let mut e = Enc::new();
    e.raw(&unreachable.encode());
    e.opt(None::<i128>, |e, c| e.offset(c));
    assert_eq!(
        state_fault(&schema, e.as_bytes()),
        Some(WireFault::NonCanonical { field: "rate_at" })
    );
    let mut bad_clock = good.clone();
    bad_clock[MomentumGlowSnapshot::LEN] = 2;
    assert_eq!(
        state_fault(&schema, &bad_clock),
        Some(WireFault::BadTag {
            field: "state.clock",
            tag: 2
        })
    );
    let mut long = good.clone();
    long.push(0);
    assert_eq!(
        state_fault(&schema, &long),
        Some(WireFault::Trailing { extra: 1 })
    );
}

/// Every refusal admission can name.
fn refusals() -> Vec<AdmitRefusal> {
    vec![
        AdmitRefusal::Tau(1),
        AdmitRefusal::Tau(2),
        AdmitRefusal::Radius(1),
        AdmitRefusal::Intensity(1),
        AdmitRefusal::Geometry(GeometryFault::CellWidthZero),
        AdmitRefusal::Geometry(GeometryFault::CellHeightZero),
        AdmitRefusal::Geometry(GeometryFault::GridWidth {
            origin_x: 1,
            cols: 2,
            cw: 3,
            limit: 4,
        }),
        AdmitRefusal::Geometry(GeometryFault::GridHeight {
            origin_y: 1,
            rows: 2,
            ch: 3,
            limit: 4,
        }),
        AdmitRefusal::Geometry(GeometryFault::HeadAboveOrigin {
            head: 1,
            origin_y: 0,
        }),
        AdmitRefusal::Geometry(GeometryFault::Unbounded),
        AdmitRefusal::GlyphSpan(5, 2),
        AdmitRefusal::FrameInterval(Duration::from_secs(2)),
        AdmitRefusal::Horizon(-1),
        AdmitRefusal::TimeWentBack { last: 2, at: 1 },
    ]
}

fn quad(row: u16, alpha: u8) -> GlowQuad {
    GlowQuad {
        row,
        x: 1,
        y: 2,
        w: 3,
        h: 4,
        color: 0x00AB_CDEF,
        alpha,
        color2: 0x0012_3456,
        alpha2: 9,
    }
}

/// Distinct observations encode to distinct bytes (the observation chain tells every pair
/// apart): each kind, each refusal variant and field, a quad moved from one stream to the
/// other, and each schedule field.
#[test]
fn distinct_observations_encode_distinctly() {
    let schema = MomentumSchema::new(Instant::now());
    let mut obs = vec![
        MomentumObservation::Keyed,
        MomentumObservation::WasReset,
        MomentumObservation::Frame {
            fill: None,
            hot: false,
            fp: 0,
            out: vec![],
            under: vec![],
        },
        MomentumObservation::Frame {
            fill: Some(0),
            hot: false,
            fp: 0,
            out: vec![],
            under: vec![],
        },
        MomentumObservation::Frame {
            fill: None,
            hot: true,
            fp: 0,
            out: vec![],
            under: vec![],
        },
        MomentumObservation::Frame {
            fill: None,
            hot: true,
            fp: 1,
            out: vec![quad(0, 1)],
            under: vec![],
        },
        MomentumObservation::Frame {
            fill: None,
            hot: true,
            fp: 1,
            out: vec![],
            under: vec![quad(0, 1)],
        },
        MomentumObservation::Frame {
            fill: None,
            hot: true,
            fp: 1,
            out: vec![quad(0, 2)],
            under: vec![],
        },
    ];
    for is_active in [false, true] {
        for settling in [false, true] {
            for cold_at in [None, Some(0), Some(1)] {
                for deadline in [None, Some(0), Some(1)] {
                    obs.push(MomentumObservation::Schedule {
                        is_active,
                        settling,
                        cold_at,
                        deadline,
                    });
                }
            }
        }
    }
    obs.extend(refusals().into_iter().map(MomentumObservation::Refused));
    let bytes: Vec<Vec<u8>> = obs.iter().map(|o| encode_observation(&schema, o)).collect();
    for i in 0..bytes.len() {
        for j in i + 1..bytes.len() {
            assert_ne!(bytes[i], bytes[j], "{:?} and {:?}", obs[i], obs[j]);
        }
    }
}

/// The artifacts carry their kinds and names at version 1; the code artifact binds the build
/// id it names (another build is another artifact), the two codes are distinct artifacts,
/// the demonstration fault's descriptor declares itself as one, and the identity puts each
/// artifact in its slot, its digest moving with the code.
#[test]
fn the_artifacts_name_their_kinds_and_bind_what_they_declare() {
    let task = task_artifact();
    assert_eq!(
        (task.kind, task.name.as_str(), task.version),
        (ArtifactKind::Task, TASK_NAME, 1)
    );
    let schema = schema_artifact();
    assert_eq!(
        (schema.kind, schema.name.as_str(), schema.version),
        (ArtifactKind::Schema, SCHEMA_NAME, 1)
    );
    assert_eq!(MomentumSchema::new(Instant::now()).id(), &schema);
    let contract = NoStrandedLight::artifact();
    assert_eq!(
        (contract.kind, contract.name.as_str(), contract.version),
        (ArtifactKind::Contract, CONTRACT_NAME, 1)
    );
    let profile = profile_artifact();
    assert_eq!(profile.kind, ArtifactKind::Profile);
    assert!(
        profile.name.starts_with(std::env::consts::ARCH),
        "{}",
        profile.name
    );
    assert!(
        profile.name.ends_with(&format!("/{NUMERIC_PROFILE}")),
        "{}",
        profile.name
    );
    assert_eq!(profile_artifact(), profile, "the profile is deterministic");

    let shipped = MomentumCode::Shipped.artifact(BUILD);
    assert_eq!(
        (shipped.kind, shipped.name.as_str(), shipped.version),
        (ArtifactKind::Code, "momentum-glow.shipped", 1)
    );
    assert_eq!(MomentumCode::Shipped.artifact(BUILD), shipped);
    assert_ne!(
        MomentumCode::Shipped.artifact("another build").digest,
        shipped.digest
    );
    let demo = MomentumCode::DemoStrandedSettle.artifact(BUILD);
    assert_eq!(demo.name, "momentum-glow.demo-stranded-settle");
    assert_ne!(demo.digest, shipped.digest);
    assert!(
        MomentumCode::DemoStrandedSettle.note().starts_with(
            "an intentionally introduced demonstration fault, not a defect of shipped code"
        ),
        "the demonstration fault declares itself"
    );

    let id: TaskIdentity = momentum_identity(&PrebuiltMomentum::new(MomentumCode::Shipped, BUILD));
    assert_eq!(
        (&id.task, &id.code, &id.contract, &id.schema, &id.profile),
        (&task, &shipped, &contract, &schema, &profile)
    );
    let demo_id = momentum_identity(&PrebuiltMomentum::new(
        MomentumCode::DemoStrandedSettle,
        BUILD,
    ));
    assert_ne!(demo_id.digest(), id.digest());
    assert_eq!(id.with_code(demo.clone()), demo_id);
}

/// Each code's short and artifact names name it, and nothing else names a code.
#[test]
fn code_names_round_trip_and_other_names_name_nothing() {
    assert_eq!(MomentumCode::ALL.len(), 2);
    for &code in MomentumCode::ALL {
        assert_eq!(MomentumCode::from_name(code.name()), Some(code));
        assert_eq!(MomentumCode::from_name(code.artifact_name()), Some(code));
        assert_eq!(PrebuiltMomentum::new(code, BUILD).code(), code);
    }
    for name in ["", "Shipped", "momentum-glow", "demo", "shipped "] {
        assert_eq!(MomentumCode::from_name(name), None, "{name:?}");
    }
}

/// While paused, keys and resets queue and ticks and queries are skipped.
#[test]
fn keys_and_resets_queue_while_paused_and_ticks_and_queries_are_skipped() {
    let schema = MomentumSchema::new(Instant::now());
    for ev in inputs() {
        let want = match ev {
            MomentumEvent::Key { .. } | MomentumEvent::Reset => PausedClass::Queue,
            MomentumEvent::Tick { .. } | MomentumEvent::Query { .. } => PausedClass::Skip,
        };
        assert_eq!(schema.paused_class(&ev), want, "{ev:?}");
    }
}

/// An unattached host's status reads unattached and empty, and an attached one names its
/// code, build and checkpoint; the status line renders either.
#[test]
fn the_status_names_the_mode_code_and_build() {
    let t0 = Instant::now();
    let plain = MomentumHost::default();
    let s = plain.aos_status();
    assert!(!s.attached && s.build.is_none() && s.checkpoint.is_none() && s.log_len == 0);
    assert_eq!(s.code, "shipped");
    assert!(
        s.to_string().starts_with("aos detached code=shipped"),
        "{s}"
    );
    let mut host = MomentumHost::default();
    host.aos_attach(t0, MomentumCode::Shipped, BUILD, 1);
    let _ = play(
        &mut host,
        t0,
        &[Call::Key {
            at: Duration::from_millis(1),
            tau: MOMENTUM_GLOW_TAU_S,
        }],
    );
    let s = host.aos_status();
    assert!(s.attached);
    assert_eq!(s.code, "shipped");
    assert_eq!(s.build.as_deref(), Some(BUILD));
    assert!(s.checkpoint.is_some());
    assert_eq!(s.log_len, 1);
    let line = s.to_string();
    assert!(
        line.starts_with(&format!("aos attached code=shipped build={BUILD} ")),
        "{line}"
    );
}

/// The quad encoding as first written: nine field writes, in field order.
fn quads_field_by_field(e: &mut Enc, quads: &[GlowQuad]) {
    e.u64(quads.len() as u64);
    for q in quads {
        e.u16(q.row);
        e.u16(q.x);
        e.u16(q.y);
        e.u16(q.w);
        e.u16(q.h);
        e.u32(q.color);
        e.u8(q.alpha);
        e.u32(q.color2);
        e.u8(q.alpha2);
    }
}

/// A quad drawn from `r`, every field over its whole range (the extremes included).
fn random_quad(r: &mut super::test_fixtures::Rng) -> GlowQuad {
    let mut field = |max: u64| match r.below(4) {
        0 => 0,
        1 => max,
        _ => r.next() & max,
    };
    GlowQuad {
        row: field(u64::from(u16::MAX)) as u16,
        x: field(u64::from(u16::MAX)) as u16,
        y: field(u64::from(u16::MAX)) as u16,
        w: field(u64::from(u16::MAX)) as u16,
        h: field(u64::from(u16::MAX)) as u16,
        color: field(u64::from(u32::MAX)) as u32,
        alpha: field(u64::from(u8::MAX)) as u8,
        color2: field(u64::from(u32::MAX)) as u32,
        alpha2: field(u64::from(u8::MAX)) as u8,
    }
}

/// Each quad is encoded as one 20-byte write; the bytes are the nine field writes' exactly,
/// for single quads and whole streams of random quads at every field's extremes, and a frame
/// observation's encoding is the frame encoder's on the same quads. Non-vacuity: a quad
/// differing in any one field encodes differently.
#[test]
fn one_twenty_byte_write_per_quad_appends_what_the_nine_field_writes_append() {
    let schema = MomentumSchema::new(Instant::now());
    let mut r = super::test_fixtures::Rng::new(0x51ab_2026_1022);
    for n in [0usize, 1, 2, 3, 7, 64, 1337] {
        for _ in 0..8 {
            let out: Vec<GlowQuad> = (0..n).map(|_| random_quad(&mut r)).collect();
            let under: Vec<GlowQuad> = (0..n / 2).map(|_| random_quad(&mut r)).collect();
            let (mut now, mut then) = (Enc::new(), Enc::new());
            super::encode_quads(&mut now, &out);
            quads_field_by_field(&mut then, &out);
            assert_eq!(now.as_bytes(), then.as_bytes(), "{n} quads");
            assert_eq!(now.len(), 8 + super::QUAD_LEN * n);
            for fill in [None, Some(0), Some(u32::MAX)] {
                let frame = crate::cursor_momentum::MomentumGlowFrame {
                    fill,
                    hot: r.below(2) == 1,
                    fp: r.next(),
                };
                let mut want = Enc::new();
                want.u8(2);
                want.opt(frame.fill, |e, f| e.u32(f));
                want.bool(frame.hot);
                want.u64(frame.fp);
                quads_field_by_field(&mut want, &out);
                quads_field_by_field(&mut want, &under);
                let mut shared = Enc::new();
                super::encode_frame(&mut shared, &frame, &out, &under);
                assert_eq!(shared.as_bytes(), want.as_bytes(), "{n} quads, {fill:?}");
                let obs = MomentumObservation::Frame {
                    fill: frame.fill,
                    hot: frame.hot,
                    fp: frame.fp,
                    out: out.clone(),
                    under: under.clone(),
                };
                assert_eq!(encode_observation(&schema, &obs), want.as_bytes());
                assert!(want.len() <= super::frame_room(out.len() + under.len()));
            }
        }
    }
    let q = random_quad(&mut r);
    let base = super::quad_bytes(&q);
    let changed = [
        GlowQuad { row: !q.row, ..q },
        GlowQuad { x: !q.x, ..q },
        GlowQuad { y: !q.y, ..q },
        GlowQuad { w: !q.w, ..q },
        GlowQuad { h: !q.h, ..q },
        GlowQuad {
            color: !q.color,
            ..q
        },
        GlowQuad {
            alpha: !q.alpha,
            ..q
        },
        GlowQuad {
            color2: !q.color2,
            ..q
        },
        GlowQuad {
            alpha2: !q.alpha2,
            ..q
        },
    ];
    for c in &changed {
        assert_ne!(super::quad_bytes(c), base, "{c:?} against {q:?}");
    }
}
