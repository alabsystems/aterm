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

    // LeaveAlternateScreen without a recorded clock fails CLOCK_RECORDED.
    let at = origin();
    let start = PaneState::new(project(&Terminal::new(6, 30)).expect("blank"), env());
    let mut unclocked = PaneRecording::new(at, start.clone());
    unclocked.bytes(Arc::from(&b"\x1b[?1049hhi"[..]), Some(sample_clock), b"");
    unclocked.host(HostOp::LeaveAlternateScreen, None);
    assert_eq!(
        law(&unclocked.check(PaneCode::ThisBuild, BUILD).verdict),
        Some((CLOCK_RECORDED, 1))
    );

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
