// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use core::time::Duration;
use std::sync::Arc;

use aterm_aos::contract::Verdict;
use aterm_aos::effects::EffectRecord;
use aterm_aos::task::{Schema, state_digest};
use aterm_aos::wire::{Dec, Enc};
use aterm_core::config::TerminalConfig;
use aterm_core::grid::ResizePolicy;
use aterm_core::terminal::{ClockReading, Rgb, Terminal};

use super::trace::{PaneRecording, PaneTrace, PaneTraceFault};
use super::*;

const BUILD: &str = "test-build";

fn origin() -> ClockReading {
    ClockReading::now()
}

fn env() -> PaneEnv {
    PaneEnv {
        config: TerminalConfig {
            default_background: Rgb {
                r: 0x20,
                g: 0x21,
                b: 0x22,
            },
            bracketed_paste: false,
            ..TerminalConfig::default()
        },
        policy: ResizePolicy::Native,
        host_resyncs_alt_screen: true,
        host_alternate_scroll: true,
        cell_px: None,
        security_policy: None,
        hyperlink_schemes: Vec::new(),
        kitty_media_admitted: false,
    }
}

/// One event of a scripted live session.
enum Live<'a> {
    Out(&'a [u8], u64),
    Resize(u16, u16),
    CellPx(u16, u16),
    Witness,
}

/// A live engine configured as a pane replays one, fed `script` from a blank screen: the recording a host makes of it, and the engine.
fn record(at: ClockReading, script: &[Live<'_>]) -> (PaneRecording, Terminal) {
    record_with_env(at, env(), script)
}

fn record_with_env(
    at: ClockReading,
    pane_env: PaneEnv,
    script: &[Live<'_>],
) -> (PaneRecording, Terminal) {
    let mut seed = Terminal::new(6, 30);
    seed.set_host_alternate_scroll(pane_env.host_alternate_scroll);
    let start = PaneState::new(project(&seed).expect("a blank screen projects"), pane_env);
    let mut rec = PaneRecording::new(at, start.clone());
    let mut live = start.engine_on(rec.schema());
    for event in script {
        match *event {
            Live::Out(bytes, ms) => {
                let clock = PaneClock {
                    offset_ns: ms * 1_000_000,
                    wall_ms: Some(1_800_000_000_000 + ms),
                };
                live.process_at(bytes, rec.schema().reading(clock));
                let reply = live.take_response().unwrap_or_default();
                rec.bytes(Arc::from(bytes), Some(clock), &reply);
            }
            Live::Resize(rows, cols) => {
                live.resize_with_policy(rows, cols, start.env().policy);
                let _ = live.take_reports();
                rec.resize(rows, cols);
            }
            Live::CellPx(width, height) => {
                live.set_cell_pixel_size(width, height);
                let _ = live.take_reports();
                rec.cell_px(width, height);
            }
            Live::Witness => {
                let seen = live.checkpoint_visible().expect("at ground");
                rec.witness(screen_digest(&seen));
            }
        }
    }
    (rec, live)
}

/// A shell prompt with its marks, a device-attributes query, a progress bar, vim's alternate screen and a resize, witnessed along the way.
fn session() -> Vec<Live<'static>> {
    vec![
        Live::Out(b"\x1b]133;A\x07$ \x1b]133;B\x07", 0),
        Live::Witness,
        Live::Out(b"ls\r\n\x1b]133;C\x07a b c\r\n\x1b]133;D;0\x07", 40),
        Live::Out(b"\x1b[c", 41),
        Live::Witness,
        Live::Out(b"[==        ] 20%\r", 100),
        Live::Out(b"[=====     ] 50%\r", 600),
        Live::Out(b"[==========] 100%\r\n", 1200),
        Live::Witness,
        Live::Out(
            b"\x1b[?1049h\x1b[2J\x1b[1;1H~\r\n~\r\n\x1b[6;1H\"f\" 2L",
            1300,
        ),
        Live::Resize(8, 40),
        Live::Witness,
        Live::Out(b"\x1b[?1049l\x1b]133;A\x07$ ", 2000),
        Live::Witness,
    ]
}

fn encoded(schema: &PaneSchema, s: &PaneState) -> Vec<u8> {
    let mut e = Enc::new();
    schema.encode_state(s, &mut e);
    e.finish()
}

#[test]
fn two_construction_paths_to_one_state_give_identical_bytes() {
    let at = origin();
    let schema = PaneSchema::new(at);
    let (rec, live) = record(at, &session());
    let continuous = PaneState::from_live(&live, &schema, env()).expect("at ground");
    let check = rec.check(PaneCode::ThisBuild, BUILD);
    assert_eq!(check.verdict, Verdict::Pass, "{check:?}");
    assert_eq!(
        check.state_digest,
        state_digest(&schema, &continuous),
        "the live engine, projected once, and the step-by-step replay from its first checkpoint"
    );
    let bytes = encoded(&schema, &continuous);
    let decoded = schema
        .decode_state(&mut Dec::new(&bytes))
        .expect("the state decodes");
    assert_eq!(encoded(&schema, &decoded), bytes, "decode then encode");

    let mut poisoned = continuous.checkpoint().clone();
    poisoned.cursor.cursor_col ^= 1;
    let poisoned = PaneState::with_carry(poisoned, continuous.carry().clone(), env());
    assert_ne!(encoded(&schema, &poisoned), bytes, "a poisoned cursor");
    let mut poisoned_carry = continuous.carry().clone();
    poisoned_carry.screen_replaced ^= 1;
    let poisoned_carry =
        PaneState::with_carry(continuous.checkpoint().clone(), poisoned_carry, env());
    assert_ne!(
        encoded(&schema, &poisoned_carry),
        bytes,
        "a poisoned step carry"
    );
    let mut other = env();
    other.config.default_background.b ^= 1;
    let other = PaneState::with_carry(
        continuous.checkpoint().clone(),
        continuous.carry().clone(),
        other,
    );
    assert_ne!(encoded(&schema, &other), bytes, "a poisoned config");
}

#[test]
fn the_session_replays_faithfully_and_counts_what_it_matched() {
    let (rec, _) = record(origin(), &session());
    let check = rec.check(PaneCode::ThisBuild, BUILD);
    assert_eq!(check.verdict, Verdict::Pass);
    assert_eq!(
        check.counts,
        PaneCounts {
            bytes: 8,
            resizes: 1,
            witnesses: 5,
            matched: 5
        }
    );
    assert_eq!(check.replies_matched, 8, "one reply effect a slice");
    assert!(
        rec.effects()[2].payload != rec.effects()[0].payload,
        "precondition: the DA query's slice recorded a reply"
    );
}

#[test]
fn a_trace_exported_and_imported_replays_to_the_same_digests() {
    let (rec, _) = record(origin(), &session());
    let here = rec.check(PaneCode::ThisBuild, BUILD);
    let file = rec
        .trace(PaneCode::ThisBuild, BUILD)
        .expect("the trace builds")
        .encode();
    let read = PaneTrace::decode(&file).expect("the file decodes");
    let later = origin();
    let there = read.check(later, BUILD).expect("the trace replays");
    assert_eq!(
        (there.start, there.state_digest, there.observation_chain),
        (here.start, here.state_digest, here.observation_chain),
        "another origin, the same digests"
    );
    assert_eq!(there.verdict, Verdict::Pass);

    let mut cut = file.clone();
    cut.pop();
    assert_eq!(
        PaneTrace::decode(&cut).map(|_| ()).unwrap_err().word(),
        "seal"
    );
    let mut other = file.clone();
    other[7] = 9;
    assert_eq!(
        PaneTrace::decode(&other).unwrap_err(),
        PaneTraceFault::Format { found: 9 }
    );
    assert!(matches!(
        read.check(later, "another-build"),
        Err(PaneTraceFault::Identity(_))
    ));
    let mut moved = read.clone();
    moved.head_chain = Digest([1; 32]);
    assert_eq!(moved.check(later, BUILD).unwrap_err().word(), "chain");
}

fn law(v: &Verdict) -> Option<(&'static str, u64)> {
    match v {
        Verdict::Pass => None,
        Verdict::Fail { law, cursor, .. } => Some((*law, *cursor)),
    }
}

#[test]
fn the_demo_clock_skew_fails_witness_matches() {
    let (rec, _) = record(origin(), &session());
    assert_eq!(law(&rec.check(PaneCode::ThisBuild, BUILD).verdict), None);
    let skewed = rec.check(PaneCode::DemoClockSkew, BUILD);
    assert_eq!(
        law(&skewed.verdict),
        Some((WITNESS_MATCHES, 1)),
        "the prompt mark carries the skewed time into the witness after it: {:?}",
        skewed.verdict
    );
    let file = rec
        .trace(PaneCode::DemoClockSkew, BUILD)
        .expect("the trace builds")
        .encode();
    let there = PaneTrace::decode(&file)
        .and_then(|t| t.check(origin(), BUILD))
        .expect("the demo's trace replays to its own head");
    assert_eq!(law(&there.verdict), Some((WITNESS_MATCHES, 1)));
}

#[test]
fn an_altered_device_attributes_reply_fails_replies_match() {
    let (rec, _) = record(origin(), &session());
    assert!(
        matches!(&rec.inputs()[3], PaneInput::Bytes { bytes, .. } if &bytes[..] == b"\x1b[c"),
        "precondition: input 3 is the DA query"
    );
    let forge = |f: &dyn Fn(&mut Vec<EffectRecord>)| {
        let mut forged = rec.clone();
        f(forged.parts_mut().1);
        law(&forged.check(PaneCode::ThisBuild, BUILD).verdict)
    };
    assert_eq!(forge(&|_| {}), None, "unaltered");
    let altered = forge(&|fx| {
        fx[2].payload = aterm_aos::effects::EffectRequest {
            target: REPLY_TARGET,
            op: REPLY_WRITE,
            payload: b"\x1b[?1;2c".to_vec(),
        }
        .payload_digest();
    });
    assert_eq!(
        altered,
        Some((REPLIES_MATCH, 3)),
        "the DA slice is the third slice, input 3"
    );
    assert_eq!(
        forge(&|fx| {
            fx.pop();
        })
        .map(|l| l.0),
        Some(REPLIES_MATCH),
        "a reply past the recording"
    );
    assert_eq!(
        forge(&|fx| fx.push(fx[0].clone())),
        Some((REPLIES_MATCH, 14)),
        "a recorded reply the replay never made"
    );
}

#[test]
fn a_dropped_clock_fails_clock_recorded() {
    let (mut rec, _) = record(origin(), &session());
    assert_eq!(law(&rec.check(PaneCode::ThisBuild, BUILD).verdict), None);
    if let PaneInput::Bytes { clock, .. } = &mut rec.parts_mut().0[2] {
        *clock = None;
    } else {
        panic!("input 2 is a slice");
    }
    let check = rec.check(PaneCode::ThisBuild, BUILD);
    assert_eq!(
        law(&check.verdict),
        Some((CLOCK_RECORDED, 2)),
        "{:?}",
        check.verdict
    );
}

#[test]
fn a_witness_inside_a_sequence_no_checkpoint_carries_is_refused_by_the_contract() {
    let (mut rec, _) = record(
        origin(),
        &[Live::Out(b"a", 0), Live::Witness, Live::Out(b"\x1bP$q", 1)],
    );
    rec.witness(Digest([0; 32]));
    let check = rec.check(PaneCode::ThisBuild, BUILD);
    match &check.verdict {
        Verdict::Fail { law, detail, .. }
            if *law == WITNESS_MATCHES && detail.contains("inside a partial sequence") => {}
        other => {
            panic!("a forged witness inside a hooked DECRQSS query fails WitnessMatches: {other:?}")
        }
    }
}

#[test]
fn the_schema_reads_back_every_input_it_writes() {
    let schema = PaneSchema::new(origin());
    for input in [
        PaneInput::Bytes {
            bytes: Arc::from(&b"x"[..]),
            clock: None,
        },
        PaneInput::Bytes {
            bytes: Arc::from(&b""[..]),
            clock: Some(PaneClock {
                offset_ns: 7,
                wall_ms: None,
            }),
        },
        PaneInput::Resize { rows: 3, cols: 4 },
        PaneInput::Witness {
            screen: Digest([9; 32]),
        },
        PaneInput::CellPx {
            width: 9,
            height: 18,
        },
    ] {
        let mut e = Enc::new();
        schema.encode_input(&input, &mut e);
        let bytes = e.finish();
        let mut d = Dec::new(&bytes);
        assert_eq!(schema.decode_input(&mut d), Ok(input));
        assert!(d.finish().is_ok());
    }
    let read = schema.reading(PaneClock {
        offset_ns: 1_500_000,
        wall_ms: Some(5),
    });
    assert_eq!(
        (read.monotonic, read.wall_ms),
        (
            schema.unclocked().monotonic + Duration::from_micros(1500),
            Some(5)
        )
    );
}

#[test]
fn an_open_mode_2026_frame_across_slices_holds_within_its_window_and_expires_past_it() {
    let at = origin();
    // 0 ms opens DEC 2026; 600 ms stays inside the 1000 ms sync timeout;
    // 1200 ms is only 600 ms after slice 1, yet 1200 ms past slice 0's open,
    // so only a carried sync_start_ns expires DEC 2026 on slice 2!
    let (rec, live) = record(
        at,
        &[
            Live::Out(b"\x1b[?2026hframe-a", 0),
            Live::Witness,
            Live::Out(b"-still-open", 600),
            Live::Witness,
            Live::Out(b"-past-window", 1200),
            Live::Witness,
        ],
    );
    assert!(
        !live.modes().synchronized_output,
        "precondition: live expired DEC 2026 at 1200 ms (> 1000 ms past 0 ms)"
    );
    let check = rec.check(PaneCode::ThisBuild, BUILD);
    assert_eq!(check.verdict, Verdict::Pass, "{check:?}");

    // Exported and imported trace at a fresh clock origin reaches the same verdict and digests.
    let file = rec
        .trace(PaneCode::ThisBuild, BUILD)
        .expect("trace builds")
        .encode();
    let rechecked = PaneTrace::decode(&file)
        .and_then(|t| t.check(origin(), BUILD))
        .expect("trace replays");
    assert_eq!(rechecked.verdict, Verdict::Pass);

    // Negative control: starting from slice 1's checkpoint with an uncarried sync_start
    // (re-armed at 600 ms instead of 0 ms) fails WitnessMatches at 1200 ms.
    let mut mid_live = Terminal::new(6, 30);
    mid_live.set_host_alternate_scroll(true);
    let schema = PaneSchema::new(at);
    mid_live.process_at(
        b"\x1b[?2026hframe-a",
        schema.reading(PaneClock {
            offset_ns: 0,
            wall_ms: Some(1_800_000_000_000),
        }),
    );
    mid_live.process_at(
        b"-still-open",
        schema.reading(PaneClock {
            offset_ns: 600_000_000,
            wall_ms: Some(1_800_000_000_600),
        }),
    );
    let mut uncarried_carry =
        PaneStepCarry::initial(&project(&mid_live).expect("at ground"), &env().config, None);
    uncarried_carry.sync_start_ns = Some(600_000_000);
    let broken_start = PaneState::with_carry(
        project(&mid_live).expect("at ground"),
        uncarried_carry,
        env(),
    );
    let mut broken = PaneRecording::new(at, broken_start);
    broken.bytes(
        Arc::from(&b"-past-window"[..]),
        Some(PaneClock {
            offset_ns: 1_200_000_000,
            wall_ms: Some(1_800_000_001_200),
        }),
        b"",
    );
    broken.witness(screen_digest(
        &live.checkpoint_visible().expect("at ground"),
    ));
    assert_eq!(
        law(&broken.check(PaneCode::ThisBuild, BUILD).verdict),
        Some((WITNESS_MATCHES, 1)),
        "without slice 0's sync_start_ns=0, 1200 ms stays open and diverges from live"
    );
}

#[test]
fn a_drained_policy_rate_limit_bucket_carries_across_slices() {
    let mut pol = aterm_core::policy::profiles::standard();
    for rl in &mut pol.rate_limits {
        if rl.id == "response" {
            rl.capacity_bytes = 24;
            rl.refill_per_second = 10;
            rl.per_sequence_max = 0;
        }
    }
    let mut pane_env = env();
    pane_env.security_policy = Some(pol);

    let at = origin();
    // 6 DSR status queries (`\x1b[5n` -> `\x1b[0n`, 4 bytes each = 24 bytes) drain the 24-byte
    // response bucket at t = 0 ms; at t = 10 ms (0.1 B refilled) the 7th query is dropped;
    // at t = 500 ms (5 B refilled) the 8th query is answered again.
    let (rec, _) = record_with_env(
        at,
        pane_env.clone(),
        &[
            Live::Out(b"\x1b[5n\x1b[5n\x1b[5n\x1b[5n\x1b[5n\x1b[5n", 0),
            Live::Out(b"\x1b[5n", 10),
            Live::Out(b"\x1b[5n", 500),
            Live::Witness,
        ],
    );
    assert_ne!(
        rec.effects()[0].payload,
        rec.effects()[1].payload,
        "precondition: slice 0 answered 6 DSRs and slice 1 dropped the 7th"
    );
    let check = rec.check(PaneCode::ThisBuild, BUILD);
    assert_eq!(check.verdict, Verdict::Pass, "{check:?}");

    // Negative control: starting slice 1 from a fresh policy bucket answers the 7th DSR
    // instead of dropping it, failing RepliesMatch.
    let fresh_start = PaneState::new(project(&Terminal::new(6, 30)).expect("blank"), pane_env);
    let mut uncarried = PaneRecording::new(at, fresh_start);
    uncarried.bytes(
        Arc::from(&b"\x1b[5n"[..]),
        Some(PaneClock {
            offset_ns: 10_000_000,
            wall_ms: Some(1_800_000_000_010),
        }),
        b"",
    );
    assert_eq!(
        law(&uncarried.check(PaneCode::ThisBuild, BUILD).verdict),
        Some((REPLIES_MATCH, 0))
    );
}

#[test]
fn a_drained_fallback_response_rate_limiter_carries_across_slices() {
    let at = origin();
    let mut seed = Terminal::new(6, 30);
    seed.set_host_alternate_scroll(true);
    let cp = project(&seed).expect("blank");
    let mut carry = PaneStepCarry::initial(&cp, &env().config, None);
    // Start with only 4 bytes left in a 24 B, 10 B/s fallback limiter at t = 0.
    carry.response_limiter = (4.0f64.to_bits(), 24, 10, Some(0));
    let start = PaneState::with_carry(cp, carry, env());
    let mut rec = PaneRecording::new(at, start.clone());
    let mut live = start.engine_on(rec.schema());

    for (bytes, ms) in [
        (&b"\x1b[5n"[..], 0u64),
        (&b"\x1b[5n"[..], 10),
        (&b"\x1b[5n"[..], 500),
    ] {
        let clock = PaneClock {
            offset_ns: ms * 1_000_000,
            wall_ms: Some(1_800_000_000_000 + ms),
        };
        live.process_at(bytes, rec.schema().reading(clock));
        let reply = live.take_response().unwrap_or_default();
        rec.bytes(Arc::from(bytes), Some(clock), &reply);
    }
    assert_eq!(rec.check(PaneCode::ThisBuild, BUILD).verdict, Verdict::Pass);
}

#[test]
fn alt_entered_without_cursor_save_and_kitty_ids_carry_across_slices() {
    // Slice 0: save DECSC at (0,0), move to (2,5), enter ?1047h (no cursor save),
    // and upload a 1x1 RGB Kitty image with i=0 so the engine assigns id=1.
    // Slice 1: leave ?1047l (must keep cursor rather than restoring DECSC (0,0))
    // and upload a second i=0 Kitty image (must reply with i=2, not i=1).
    let (rec, _) = record(
        origin(),
        &[
            Live::Out(
                b"\x1b[1;1H\x1b7\x1b[3;6H\x1b[?1047h\x1b_Ga=t,f=24,s=1,v=1;////\x1b\\",
                0,
            ),
            Live::Witness,
            Live::Out(b"\x1b[?1047lX\x1b_Ga=t,f=24,s=1,v=1;////\x1b\\", 20),
            Live::Witness,
        ],
    );
    let check = rec.check(PaneCode::ThisBuild, BUILD);
    assert_eq!(check.verdict, Verdict::Pass, "{check:?}");
}

#[test]
fn cell_px_and_hyperlink_schemes_replay_across_slices() {
    let mut pane_env = env();
    pane_env.config.allow_window_ops = true;
    pane_env.cell_px = Some((9, 18));
    pane_env.security_policy = Some(aterm_core::policy::profiles::permissive());
    pane_env.hyperlink_schemes = vec!["orca".to_string()];

    let (rec, _) = record_with_env(
        origin(),
        pane_env,
        &[
            Live::Out(
                b"\x1b[16t\x1b[14t\x1b]8;;orca://pkg/item\x07link\x1b]8;;\x07",
                0,
            ),
            Live::Witness,
            Live::CellPx(10, 20),
            Live::Out(b"\x1b[16t\x1b[14t", 30),
            Live::Witness,
        ],
    );
    let check = rec.check(PaneCode::ThisBuild, BUILD);
    assert_eq!(check.verdict, Verdict::Pass, "{check:?}");
    let file = rec
        .trace(PaneCode::ThisBuild, BUILD)
        .expect("trace builds")
        .encode();
    let rechecked = PaneTrace::decode(&file)
        .and_then(|t| t.check(origin(), BUILD))
        .expect("trace replays");
    assert_eq!(rechecked.verdict, Verdict::Pass);
}

#[test]
fn host_ops_roundtrip_and_enforce_clock_and_canonical_state_laws() {
    use aterm_core::terminal::{CutKind, HolderCut, HostOp, ShellIntegrationNonce};

    let schema = PaneSchema::new(origin());
    let sample_clock = PaneClock {
        offset_ns: 25_000_000,
        wall_ms: Some(1_800_000_000_025),
    };
    let all_ops = [
        (
            HostOp::ForegroundJob {
                active: true,
                probed: false,
                restore_cwd: false,
            },
            None,
        ),
        (
            HostOp::ForegroundJob {
                active: false,
                probed: true,
                restore_cwd: false,
            },
            None,
        ),
        (
            HostOp::ForegroundJob {
                active: false,
                probed: false,
                restore_cwd: true,
            },
            None,
        ),
        (
            HostOp::HolderCut(HolderCut {
                to: Some(7),
                kind: CutKind::Group,
                from_alive: true,
            }),
            None,
        ),
        (
            HostOp::HolderCut(HolderCut {
                to: None,
                kind: CutKind::Exec,
                from_alive: false,
            }),
            None,
        ),
        (HostOp::HolderRetire(7), None),
        (HostOp::LeaveAlternateScreen, Some(sample_clock)),
        (HostOp::ExpectHostAltLeave, None),
        (HostOp::ClearScrollback, None),
        (HostOp::Reset, None),
        (HostOp::AuthorizeShell([0xAA; 32]), None),
        (HostOp::AuthorizeShellOnFirstMark([0xBB; 32]), None),
        (HostOp::AuthorizeShellRekey([0xCC; 32]), None),
        (HostOp::WithdrawShellRekey([0xCC; 32]), None),
        (HostOp::SettleShellRekey([0xCC; 32]), None),
        (HostOp::AdvanceKittyAnimations, Some(sample_clock)),
    ];
    for (op, clock) in all_ops {
        let input = PaneInput::Host { op, clock };
        let mut e = Enc::new();
        schema.encode_input(&input, &mut e);
        let bytes = e.finish();
        let mut d = Dec::new(&bytes);
        let decoded = schema
            .decode_input(&mut d)
            .expect("canonical HostOp decodes");
        d.finish().expect("no trailing bytes");
        assert_eq!(decoded, input);

        let obs = PaneObservation::Host {
            op,
            clocked: clock.is_some(),
        };
        let mut oe = Enc::new();
        schema.encode_observation(&obs, &mut oe);
        let obytes = oe.finish();
        assert_eq!(obytes.first(), Some(&4));
    }

    // Non-canonical ForegroundJob combinations (active && probed, or restore_cwd while active) fail closed.
    for bad in [
        [4u8, 0, 1, 1, 0, 0],
        [4u8, 0, 1, 0, 1, 0],
        [4u8, 0, 0, 1, 1, 0],
    ] {
        let mut d = Dec::new(&bad);
        assert!(
            matches!(
                schema.decode_input(&mut d),
                Err(aterm_aos::wire::WireFault::NonCanonical {
                    field: "input.host.foreground_job"
                })
            ),
            "bad encoding {bad:?} must reject"
        );
    }

    // LeaveAlternateScreen and AdvanceKittyAnimations without a recorded clock fail CLOCK_RECORDED.
    let at = origin();
    let start = PaneState::new(project(&Terminal::new(6, 30)).expect("blank"), env());
    for op in [HostOp::LeaveAlternateScreen, HostOp::AdvanceKittyAnimations] {
        let mut unclocked = PaneRecording::new(at, start.clone());
        unclocked.bytes(Arc::from(&b"\x1b[?1049hhi"[..]), Some(sample_clock), b"");
        unclocked.host(op, None);
        assert_eq!(
            law(&unclocked.check(PaneCode::ThisBuild, BUILD).verdict),
            Some((CLOCK_RECORDED, 1)),
            "{op:?} must require a clock"
        );
    }

    // State invariant: awaiting_shell_nonce and checkpoint.shell_integration_nonce cannot both be Some.
    let mut cp = start.checkpoint().clone();
    cp.shell_integration_nonce = Some(ShellIntegrationNonce([0x11; 32]));
    let mut carry = start.carry().clone();
    carry.awaiting_shell_nonce = Some(ShellIntegrationNonce([0x22; 32]));
    let bad_state = PaneState::with_carry(cp, carry, env());
    let mut se = Enc::new();
    schema.encode_state(&bad_state, &mut se);
    let sbytes = se.finish();
    let mut sd = Dec::new(&sbytes);
    assert!(matches!(
        schema.decode_state(&mut sd),
        Err(aterm_aos::wire::WireFault::NonCanonical {
            field: "state.carry.awaiting_shell_nonce"
        })
    ));
}

#[test]
fn split_decrqss_and_xtgettcap_queries_carry_and_answer_across_slices() {
    let streams: &[&[u8]] = &[
        b"\x1b[31m\x1bP$qm\x1b\\ok",
        b"\x1bP+q544E\x1b\\ok",
        b"\x1bP$qm\x1b[1;3Hok",
        b"\x1bPtmux;\x1b\x1bP$qm\x1b\x1b\\\x1b\\ok",
    ];
    for &stream in streams {
        for cut in 1..stream.len() {
            let (head, tail) = stream.split_at(cut);
            let (rec, _) = record(
                origin(),
                &[Live::Out(head, 10), Live::Out(tail, 20), Live::Witness],
            );
            let check = rec.check(PaneCode::ThisBuild, BUILD);
            assert_eq!(
                check.verdict,
                Verdict::Pass,
                "cut {cut} in {:?}: {check:?}",
                String::from_utf8_lossy(stream)
            );
            assert_eq!(check.replies_matched, 2);
        }
    }

    // Over-cap DECRQSS (>256 bytes) and XTGETTCAP (>1024 bytes), mid-query resize, and PaneTrace round-trip.
    let mut long_decrqss = b"\x1bP$q".to_vec();
    long_decrqss.extend(std::iter::repeat_n(b'x', 300));
    let mut long_xtgettcap = b"\x1bP+q".to_vec();
    long_xtgettcap.extend(b"544E;".iter().copied().cycle().take(1200));
    let (rec, _) = record(
        origin(),
        &[
            Live::Out(&long_decrqss[..150], 10),
            Live::Resize(8, 40),
            Live::Out(&long_decrqss[150..], 20),
            Live::Out(b"\x1b\\after1\r\n", 30),
            Live::Witness,
            Live::Out(&long_xtgettcap[..600], 40),
            Live::Out(&long_xtgettcap[600..], 50),
            Live::Out(b"\x1b\\after2", 60),
            Live::Witness,
        ],
    );
    let here = rec.check(PaneCode::ThisBuild, BUILD);
    assert_eq!(here.verdict, Verdict::Pass, "{here:?}");
    let encoded = rec
        .trace(PaneCode::ThisBuild, BUILD)
        .expect("trace builds")
        .encode();
    let decoded = PaneTrace::decode(&encoded).expect("trace decodes");
    let there = decoded.check(origin(), BUILD).expect("trace replays");
    assert_eq!(there.verdict, Verdict::Pass);
    assert_eq!(there.state_digest, here.state_digest);

    // State invariant: dcs_query requires !checkpoint.parser_ground && checkpoint.parser.is_none().
    let schema = PaneSchema::new(origin());
    let mut mid_term = Terminal::new(6, 30);
    mid_term.process(b"\x1bP$qm");
    let valid_carry = PaneStepCarry::capture(&mid_term, origin().monotonic, PANE_HISTORY_LINES);
    assert!(valid_carry.dcs_query.is_some());
    let ground_cp = project(&Terminal::new(6, 30)).expect("ground");
    let bad_state = PaneState::with_carry(ground_cp, valid_carry, env());
    let mut se = Enc::new();
    schema.encode_state(&bad_state, &mut se);
    let sbytes = se.finish();
    let mut sd = Dec::new(&sbytes);
    assert!(matches!(
        schema.decode_state(&mut sd),
        Err(aterm_aos::wire::WireFault::NonCanonical {
            field: "state.carry.dcs_query"
        })
    ));
}

#[test]
fn stored_kitty_images_numbers_virtuals_and_chunked_transfers_replay_across_slices() {
    let at = origin();
    let schema = PaneSchema::new(at);
    let mut pane_env = env();
    pane_env.cell_px = Some((10, 20));
    let mut seed = Terminal::new(6, 30);
    seed.set_host_alternate_scroll(pane_env.host_alternate_scroll);
    seed.set_cell_pixel_size(10, 20);
    let start = PaneState::new(project(&seed).expect("blank screen"), pane_env.clone());
    let mut rec = PaneRecording::new(at, start.clone());
    let mut live = start.engine_on(rec.schema());

    let push_slice = |rec: &mut PaneRecording, live: &mut Terminal, bytes: &[u8], ms: u64| {
        let clock = PaneClock {
            offset_ns: ms * 1_000_000,
            wall_ms: Some(1_800_000_000_000 + ms),
        };
        live.process_at(bytes, rec.schema().reading(clock));
        let reply = live.take_response().unwrap_or_default();
        rec.bytes(Arc::from(bytes), Some(clock), &reply);
        reply
    };

    // Slice 1: store RGBA image 1 via number 42 + second animation frame.
    push_slice(
        &mut rec,
        &mut live,
        b"\x1b_Ga=t,f=32,s=3,v=1,I=42;////AAAA////AAAA\x1b\\\
          \x1b_Ga=f,f=32,s=3,v=1,i=1,z=50;AAAA////AAAA////\x1b\\",
        10,
    );
    // Slice 2: create U=1 virtual placement + Unicode placeholder, and open an m=1 chunked transfer.
    let mut slice2 = b"\x1b_Ga=p,U=1,i=1,p=3,c=2,r=1\x1b\\".to_vec();
    slice2.extend_from_slice("\x1b[2;1H\x1b[38;5;1m\u{10EEEE}\u{0305}\u{0305}\x1b[39m".as_bytes());
    slice2.extend_from_slice(b"\x1b_Ga=t,f=24,s=1,v=1,i=8,m=1;/w\x1b\\");
    push_slice(&mut rec, &mut live, &slice2, 20);

    // Capture a mid-recording state for the negative control before Slice 3 completes and places.
    let mid_cp = project(&live).expect("ground between chunks");
    let mid_carry = PaneStepCarry::capture(&live, at.monotonic, PANE_HISTORY_LINES);
    assert_eq!(mid_carry.kitty_store.len(), 1);
    assert_eq!(mid_carry.kitty_numbers, vec![(42, 1)]);
    assert!(mid_carry.kitty_pending.is_some());

    // Slice 3: finish m=0 chunked transfer, place by I=42 and i=8, switch animation frame, witness.
    let slice3 = b"\x1b_Gm=0;AA\x1b\\\
                   \x1b[3;1H\x1b_Ga=p,I=42,c=2,r=1\x1b\\\
                   \x1b[4;1H\x1b_Ga=p,i=8,c=1,r=1\x1b\\\
                   \x1b_Ga=a,i=1,c=2\x1b\\";
    let slice3_reply = push_slice(&mut rec, &mut live, slice3, 30);
    let seen3 = live.checkpoint_visible().expect("at ground");
    rec.witness(screen_digest(&seen3));

    // Slice 4: delete all Kitty images (`d=A`) and witness again.
    push_slice(&mut rec, &mut live, b"\x1b_Ga=d,d=A\x1b\\", 40);
    let seen4 = live.checkpoint_visible().expect("at ground");
    rec.witness(screen_digest(&seen4));

    let here = rec.check(PaneCode::ThisBuild, BUILD);
    assert_eq!(here.verdict, Verdict::Pass, "{here:?}");
    assert_eq!(here.counts.matched, 2);
    assert!(here.replies_matched >= 3);

    let continuous = PaneState::from_live(&live, &schema, pane_env.clone()).expect("at ground");
    assert_eq!(here.state_digest, state_digest(&schema, &continuous));

    let encoded_trace = rec
        .trace(PaneCode::ThisBuild, BUILD)
        .expect("trace builds")
        .encode();
    let decoded = PaneTrace::decode(&encoded_trace).expect("trace decodes");
    let there = decoded.check(at, BUILD).expect("trace replays");
    assert_eq!(there.verdict, Verdict::Pass);
    assert_eq!(there.state_digest, here.state_digest);

    // Negative control: without the carried Kitty store/numbers/pending state, Slice 3 answers
    // ENOENT instead of OK and fails REPLIES_MATCH.
    let mut stripped_carry = mid_carry;
    stripped_carry.kitty_store.clear();
    stripped_carry.kitty_numbers.clear();
    stripped_carry.kitty_pending = None;
    let mut broken =
        PaneRecording::new(at, PaneState::with_carry(mid_cp, stripped_carry, pane_env));
    broken.bytes(
        Arc::from(&slice3[..]),
        Some(PaneClock {
            offset_ns: 30_000_000,
            wall_ms: Some(1_800_000_000_030),
        }),
        &slice3_reply,
    );
    broken.witness(screen_digest(&seen3));
    let failed = broken.check(PaneCode::ThisBuild, BUILD);
    assert!(
        law(&failed.verdict).is_some(),
        "dropping Kitty carry must fail replay check: {failed:?}"
    );
}

#[test]
fn placed_inline_images_relative_floating_and_history_replay_across_slices() {
    let at = origin();
    let schema = PaneSchema::new(at);
    let mut pane_env = env();
    pane_env.cell_px = Some((10, 20));
    let mut live = Terminal::new(6, 30);
    live.set_host_alternate_scroll(pane_env.host_alternate_scroll);
    live.set_cell_pixel_size(10, 20);

    // Pre-keyframe output: loose OSC 1337 spanning scrolled history + visible row, unnamed `a=T`
    // (`i=0`), stored 2-frame Kitty `i=1` with cell parent `p=10` + relative child `p=11`, and
    // virtual `p=2` + placeholder + floating child `p=21`.
    live.process(
        b"\x1b]1337;File=inline=1;width=3;height=2;preserveAspectRatio=0:QUJDREVGRw==\x1b\\\
          \x1b[6;1H\r\n\
          \x1b[1;12H\x1b_Ga=T,f=24,s=1,v=1,c=2,r=1;/wAA\x1b\\\
          \x1b_Ga=t,f=32,s=2,v=1,i=1;////AAAA//8=\x1b\\\
          \x1b_Ga=f,f=32,s=2,v=1,i=1,z=40;AAAA//////8=\x1b\\\
          \x1b[3;2H\x1b_Ga=p,i=1,p=10,c=2,r=1\x1b\\\
          \x1b_Ga=p,i=1,p=11,P=1,Q=10,H=3,V=0,c=1,r=1\x1b\\\
          \x1b_Ga=p,U=1,i=1,p=2,c=2,r=1\x1b\\",
    );
    live.process("\x1b[4;2H\x1b[38;5;1;58;5;2m\u{10EEEE}\u{0305}\u{0305}\x1b[39;59m".as_bytes());
    live.process(b"\x1b_Ga=p,i=1,p=21,P=1,Q=2,H=1,V=0,c=1,r=1\x1b\\");
    let _ = live.take_response();

    // Seed a mid-session recording from `PaneState::from_live`.
    let start = PaneState::from_live(&live, &schema, pane_env.clone()).expect("at ground");
    assert_eq!(start.carry().loose_images.len(), 2);
    assert_eq!(start.carry().kitty_relations.len(), 1);
    assert_eq!(start.carry().kitty_floating.len(), 1);
    assert!(!start.carry().active_images.visible.is_empty());
    assert!(!start.carry().active_images.history_rev.is_empty());

    let mut rec = PaneRecording::new(at, start.clone());
    let push_slice = |rec: &mut PaneRecording, live: &mut Terminal, bytes: &[u8], ms: u64| {
        let clock = PaneClock {
            offset_ns: ms * 1_000_000,
            wall_ms: Some(1_800_000_000_000 + ms),
        };
        live.process_at(bytes, rec.schema().reading(clock));
        let reply = live.take_response().unwrap_or_default();
        rec.bytes(Arc::from(bytes), Some(clock), &reply);
        reply
    };

    // Slice 1: attach a grandchild cell placement (`P=1,Q=11`) to pre-keyframe `p=11`, attach a
    // grandchild floating placement (`P=1,Q=21`) to pre-keyframe `p=21`, move root `p=10` (which
    // cascades to `p=11` and `p=12`), and switch animation frame 2.
    let slice1 = b"\x1b_Ga=p,i=1,p=12,P=1,Q=11,H=1,V=0,c=1,r=1\x1b\\\
                   \x1b_Ga=p,i=1,p=22,P=1,Q=21,H=1,V=0,c=1,r=1\x1b\\\
                   \x1b[3;6H\x1b_Ga=p,i=1,p=10,c=2,r=1\x1b\\\
                   \x1b_Ga=a,i=1,c=2\x1b\\";
    let slice1_reply = push_slice(&mut rec, &mut live, slice1, 10);
    let seen1 = live.checkpoint_visible().expect("at ground");
    rec.witness(screen_digest(&seen1));

    // Slice 2: cascading delete of `p=10` (`d=P`) removes `p=10`, `p=11`, and `p=12`.
    push_slice(&mut rec, &mut live, b"\x1b_Ga=d,d=P,i=1,p=10\x1b\\", 20);
    let seen2 = live.checkpoint_visible().expect("at ground");
    rec.witness(screen_digest(&seen2));

    let here = rec.check(PaneCode::ThisBuild, BUILD);
    assert_eq!(here.verdict, Verdict::Pass, "{here:?}");
    assert_eq!(here.counts.matched, 2);

    let continuous = PaneState::from_live(&live, &schema, pane_env.clone()).expect("at ground");
    assert_eq!(here.state_digest, state_digest(&schema, &continuous));

    let encoded_trace = rec
        .trace(PaneCode::ThisBuild, BUILD)
        .expect("trace builds")
        .encode();
    let decoded = PaneTrace::decode(&encoded_trace).expect("trace decodes");
    let there = decoded.check(at, BUILD).expect("trace replays");
    assert_eq!(there.verdict, Verdict::Pass);
    assert_eq!(there.state_digest, here.state_digest);

    // Negative control: without placed-grid / floating / relation carry on the keyframe, `Q=11`
    // and `Q=21` fail with ENOPARENT and trip REPLIES_MATCH.
    let mut stripped_carry = start.carry().clone();
    stripped_carry.loose_images.clear();
    stripped_carry.kitty_floating.clear();
    stripped_carry.kitty_relations.clear();
    stripped_carry.active_images = Default::default();
    let mut broken = PaneRecording::new(
        at,
        PaneState::with_carry(start.checkpoint().clone(), stripped_carry, pane_env),
    );
    broken.bytes(
        Arc::from(&slice1[..]),
        Some(PaneClock {
            offset_ns: 10_000_000,
            wall_ms: Some(1_800_000_000_010),
        }),
        &slice1_reply,
    );
    broken.witness(screen_digest(&seen1));
    let failed = broken.check(PaneCode::ThisBuild, BUILD);
    assert!(
        law(&failed.verdict).is_some(),
        "dropping placed image carry must fail replay check: {failed:?}"
    );
}

#[test]
fn kitty_animation_frame_steps_and_finite_loops_replay_across_slices_and_roundtrip_trace() {
    let at = origin();
    let schema = PaneSchema::new(at);
    let mut pane_env = env();
    pane_env.cell_px = Some((10, 20));
    let mut live = Terminal::new(6, 30);
    live.set_cell_pixel_size(10, 20);
    let start = PaneState::from_live(&live, &schema, pane_env.clone()).expect("at ground");
    let mut rec = PaneRecording::new(at, start.clone());

    // Load a 2-frame 2x1 RGBA animation with `v=2` (frame 1 -> frame 2 -> rests on frame 2).
    let setup = b"\x1b_Ga=t,f=32,s=2,v=1,i=7,q=2;////AAAA//8=\x1b\\\
                  \x1b_Ga=f,f=32,s=2,v=1,i=7,z=30,q=2;AAAA//////8=\x1b\\\
                  \x1b_Ga=a,i=7,r=1,z=30\x1b\\\
                  \x1b_Ga=p,i=7,c=1,r=1,C=1,q=2\x1b\\\
                  \x1b_Ga=a,i=7,s=3,v=2\x1b\\";
    let t0 = PaneClock {
        offset_ns: 10_000_000,
        wall_ms: None,
    };
    live.process_at(setup, schema.reading(t0));
    rec.bytes(Arc::from(&setup[..]), Some(t0), b"");

    // Autonomous tick 1 at +30ms: advances to frame 2.
    let tick1 = PaneClock {
        offset_ns: 40_000_000,
        wall_ms: None,
    };
    let step1 = live.advance_kitty_animations(schema.reading(tick1).monotonic);
    assert!(step1.advanced);
    rec.host(HostOp::AdvanceKittyAnimations, Some(tick1));
    rec.witness(screen_digest(&live.checkpoint_visible().expect("ground")));

    // Autonomous tick 2 at +60ms: finite `v=2` reaches `loops == 1` and rests without changing
    // the shown frame (`!step2.advanced`), yet mutates `animation.loops` in canonical state!
    let tick2 = PaneClock {
        offset_ns: 70_000_000,
        wall_ms: None,
    };
    let step2 = live.advance_kitty_animations(schema.reading(tick2).monotonic);
    assert!(!step2.advanced && step2.next_frame_at.is_none());
    rec.host(HostOp::AdvanceKittyAnimations, Some(tick2));

    let here = rec.check(PaneCode::ThisBuild, BUILD);
    assert_eq!(here.verdict, Verdict::Pass, "{here:?}");
    let end_state = PaneState::from_live(&live, &schema, pane_env).expect("ground");
    assert_eq!(here.state_digest, state_digest(&schema, &end_state));

    let encoded_trace = rec
        .trace(PaneCode::ThisBuild, BUILD)
        .expect("trace builds")
        .encode();
    let decoded = PaneTrace::decode(&encoded_trace).expect("trace decodes");
    let there = decoded.check(at, BUILD).expect("trace replays");
    assert_eq!(there.verdict, Verdict::Pass);
    assert_eq!(there.state_digest, here.state_digest);

    // Negative control: omitting the resting finite-loop tick leaves `animation.loops == 0`
    // instead of `1`, changing `state_digest`.
    let mut missing_rest_tick = PaneRecording::new(at, start);
    missing_rest_tick.bytes(Arc::from(&setup[..]), Some(t0), b"");
    missing_rest_tick.host(HostOp::AdvanceKittyAnimations, Some(tick1));
    let missing_check = missing_rest_tick.check(PaneCode::ThisBuild, BUILD);
    assert_ne!(missing_check.state_digest, here.state_digest);
}

#[test]
fn kitty_media_admitted_replays_file_queries_and_refused_transmits_across_slices() {
    let at = origin();
    let schema = PaneSchema::new(at);
    let mut pane_env = env();
    pane_env.kitty_media_admitted = true;

    let (rec, live) = record_with_env(
        at,
        pane_env.clone(),
        &[
            Live::Out(
                b"\x1b_Gi=31,a=q,t=f\x1b\\\
                  \x1b_Gi=32,a=q,t=t\x1b\\\
                  \x1b_Gi=33,a=q,t=s\x1b\\",
                10,
            ),
            Live::Out(b"\x1b_Gi=34,a=T,t=f,f=32,s=1,v=1;L21pc3Npbmc=\x1b\\", 20),
            Live::Witness,
        ],
    );
    let here = rec.check(PaneCode::ThisBuild, BUILD);
    assert_eq!(here.verdict, Verdict::Pass, "{here:?}");
    let end_state = PaneState::from_live(&live, &schema, pane_env).expect("ground");
    assert_eq!(here.state_digest, state_digest(&schema, &end_state));

    let encoded_trace = rec
        .trace(PaneCode::ThisBuild, BUILD)
        .expect("trace builds")
        .encode();
    let decoded = PaneTrace::decode(&encoded_trace).expect("trace decodes");
    let there = decoded.check(at, BUILD).expect("trace replays");
    assert_eq!(there.verdict, Verdict::Pass);
    assert_eq!(there.state_digest, here.state_digest);

    // Negative control: without `kitty_media_admitted`, slice 0 answers ENOTSUPPORTED instead of OK.
    let start_disabled = PaneState::new(project(&Terminal::new(6, 30)).expect("blank"), env());
    let mut broken = PaneRecording::new(at, start_disabled);
    broken.bytes(
        Arc::from(&b"\x1b_Gi=31,a=q,t=f\x1b\\"[..]),
        Some(PaneClock {
            offset_ns: 10_000_000,
            wall_ms: Some(1_800_000_000_010),
        }),
        b"\x1b_Gi=31;OK\x1b\\",
    );
    assert_eq!(
        law(&broken.check(PaneCode::ThisBuild, BUILD).verdict),
        Some((REPLIES_MATCH, 0))
    );
}

#[test]
fn extreme_clock_offsets_saturate_monotonic_instant_without_panicking() {
    let at = origin();
    let schema = PaneSchema::new(at);
    let near_max = schema.reading(PaneClock {
        offset_ns: u64::MAX - 1,
        wall_ms: Some(u64::MAX - 1),
    });
    let at_max = schema.reading(PaneClock {
        offset_ns: u64::MAX,
        wall_ms: Some(u64::MAX),
    });
    assert!(near_max.monotonic >= at.monotonic);
    assert!(at_max.monotonic >= near_max.monotonic);
    assert_eq!(at_max.wall_ms, Some(u64::MAX));

    let start = PaneState::new(project(&Terminal::new(4, 20)).expect("blank"), env());
    let mut rec = PaneRecording::new(at, start);
    rec.bytes(
        Arc::from(&b"ok"[..]),
        Some(PaneClock {
            offset_ns: u64::MAX,
            wall_ms: Some(u64::MAX),
        }),
        b"",
    );
    assert_eq!(rec.check(PaneCode::ThisBuild, BUILD).verdict, Verdict::Pass);
    let _ = rec.check(PaneCode::DemoClockSkew, BUILD);

    let encoded = rec
        .trace(PaneCode::ThisBuild, BUILD)
        .expect("trace")
        .encode();
    let decoded = PaneTrace::decode(&encoded).expect("decodes");
    assert_eq!(
        decoded.check(at, BUILD).expect("replays").verdict,
        Verdict::Pass
    );
}

#[test]
fn judge_refuses_surplus_observations_and_refused_cell_px_keeps_state_env() {
    let at = origin();
    let schema = PaneSchema::new(at);
    let contract = FaithfulReplay::new();
    let state = PaneState::new(project(&Terminal::new(4, 20)).expect("blank"), env());
    let run = aterm_aos::replay::Run {
        state,
        state_digest: Digest([0; 32]),
        observations: vec![(0, PaneObservation::Resized { rows: 4, cols: 20 })],
        observation_chain: Digest([0; 32]),
        refused: None,
    };
    assert!(matches!(
        contract.judge(&schema, &[], &run),
        Verdict::Fail {
            cursor: 0,
            law: WITNESS_MATCHES,
            ..
        }
    ));

    let mut state = PaneState::new(project(&Terminal::new(4, 20)).expect("blank"), env());
    let code = PrebuiltPane::new(PaneCode::ThisBuild, BUILD);
    let err = code
        .step(
            &schema,
            &mut state,
            &PaneInput::CellPx {
                width: 0,
                height: 18,
            },
            &mut aterm_aos::effects::NoEffects,
        )
        .expect_err("zero-width CellPx must refuse with RESIZE_GEOMETRY");
    assert_eq!(err.law, RESIZE_GEOMETRY);
    assert_eq!(
        state.env().cell_px,
        None,
        "refused CellPx step must not mutate state.env"
    );

    let err_partial = code
        .step(
            &schema,
            &mut state,
            &PaneInput::Bytes {
                bytes: Arc::from(&b"\x1bP1$t"[..]),
                clock: Some(PaneClock {
                    offset_ns: 0,
                    wall_ms: None,
                }),
            },
            &mut aterm_aos::effects::NoEffects,
        )
        .expect_err("uncarried DECRSPS partial sequence must refuse step");
    assert_eq!(err_partial.law, UNCARRIED_PARTIAL);
}
