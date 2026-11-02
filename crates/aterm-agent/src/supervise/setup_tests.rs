// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`SessionSetup`].

use super::*;
use crate::supervise::tape::header::{Entry, Header};

struct NoCtl;

impl Ctl for NoCtl {
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        panic!("a setup test sent {args:?}");
    }
}

fn every_field_moved(dir: &Path) -> SessionSetup {
    let d = |n: u64| Duration::new(n, 7);
    SessionSetup {
        sid: Some("@s-odd".to_string()),
        holder: "odd-holder".to_string(),
        beside: Some("codex-alab:7".to_string()),
        handover: Handover(Some(Arc::new(AtomicBool::new(true)))),
        types_under_hand: true,
        builtin_duties: true,
        confirms_agent: true,
        ledger_path: Some(dir.join("drive").join("s-odd.jsonl")),
        approval_env: ApprovalEnv {
            home: Some(dir.join("home")),
            uid: 4242,
            tmpdir: Some(dir.join("tmp")),
            worker: WorkerSource::Session,
        },
        reconnect: d(1),
        pause: d(2),
        pause_max: d(3),
        claim_renew: d(4),
        claim_ttl_ms: 12_345,
        redecide_after_skip: d(54),
        turn_end_timing: TurnEndTiming {
            min_work: d(5),
            short_backoff: d(6),
            short_backoff_max: d(7),
            retry_backoff: vec![d(8), d(9)],
            net_backoff: vec![d(10)],
            outage_first: d(11),
            outage_cap: d(60),
            progress: d(12),
            reset_grace: d(13),
            reset_jitter: d(14),
            reset_minute: d(61),
            auto_resume_grace: d(15),
            limit_backoff: vec![d(16), d(17), d(18)],
            entitlement_backoff: vec![],
            refused_backoff: vec![d(54), d(55)],
            window: d(19),
            take_within: d(20),
            restart_backoff: vec![d(21)],
            credits_bound: d(57),
        },
        press_badge_after: d(27),
        survey_retry: [d(28), d(29), d(30), d(31)],
        survey_settle: d(32),
        survey_land: d(33),
        probe_waits: ProbeWaits {
            open: d(34),
            settle: d(35),
            step: d(36),
            close: d(37),
        },
        reset_waits: reset_loop::ResetWaits {
            open: d(38),
            check: d(39),
            claim: d(40),
            line: d(41),
            again: d(42),
            enter: d(43),
            enter_by: d(44),
            step: d(45),
            close: d(46),
        },
        box_settle: d(47),
        autocompact_settle: d(48),
        background_settle: d(49),
        wait_step: d(58),
        box_call_step: d(56),
        stuck_after: d(50),
        person_hold_wait: Some(d(52)),
        beside_idle_min: d(53),
        occupancy_nudge_after: d(59),
        occupancy_k_looks: 3,
    }
}

fn setup_lines(text: &str) -> Vec<&str> {
    text.lines()
        .skip_while(|l| *l != "[setup]")
        .take_while(|l| *l != "[opts]")
        .collect()
}

#[test]
fn every_field_of_the_setup_comes_back_from_the_header() {
    let dir = Path::new(if cfg!(windows) {
        r"C:\odd-root"
    } else {
        "/odd-root"
    });
    let odd = every_field_moved(dir);
    let opts = SuperviseOpts::default();
    let text = Header::capture("t-1", Entry::Watch, Some(dir), &odd, &opts).encode();
    let back = Header::decode(&text).expect("a header this build wrote");
    assert_eq!(back.setup, odd);
    assert!(back.setup.handover.0.is_some(), "the flag's presence");
    assert!(
        !back.setup.handover.raised(),
        "a fresh flag, not the host's"
    );

    let shipped = SessionSetup::new(None);
    // The private adapter's opt-ins and a scaled reset minute round-trip above, but their shipped
    // values are deliberately omitted so pre-existing setup bytes stay unchanged.
    let omitted = [
        "builtin_duties=",
        "confirms_agent=",
        "turn_end.reset_minute=",
    ];
    let ours: Vec<_> = setup_lines(&text)
        .into_iter()
        .filter(|line| !omitted.iter().any(|k| line.starts_with(k)))
        .collect();
    let theirs_text = Header::capture("t-1", Entry::Watch, Some(dir), &shipped, &opts).encode();
    let theirs = setup_lines(&theirs_text);
    assert!(
        !theirs
            .iter()
            .any(|line| omitted.iter().any(|k| line.starts_with(k)))
    );
    assert_eq!(ours.len(), theirs.len(), "{ours:#?}\n{theirs:#?}");
    for (a, b) in ours.iter().zip(&theirs) {
        if a.starts_with('[') {
            assert_eq!(a, b);
        } else {
            assert_ne!(a, b, "a field left at its shipped value");
        }
    }
}

#[test]
fn the_setters_write_the_setup() {
    let mut ctl = NoCtl;
    let mut s = Session::new(&mut ctl, Some("@s-1".to_string()));
    assert_eq!(s.setup(), &SessionSetup::new(Some("@s-1".to_string())));
    assert_eq!(
        s.setup().holder(),
        format!("aterm-supervise:{}", std::process::id())
    );
    s.set_supervisor_name(Some("aterm-gui:9".to_string()));
    assert_eq!(s.setup().holder(), "aterm-gui:9");
    s.set_supervisor_name(None);
    assert_eq!(s.setup().holder(), claim::default_holder());

    let timing = TurnEndTiming {
        min_work: Duration::from_secs(1),
        ..TurnEndTiming::default()
    };
    s.set_turn_end_timing(timing.clone());
    assert_eq!(s.setup.turn_end_timing, timing);

    s.beside_idle_at = Some((7, 1, Instant::now()));
    s.set_beside(Some("codex-alab:7".to_string()));
    assert_eq!(s.setup.beside.as_deref(), Some("codex-alab:7"));
    assert_eq!(
        s.beside_idle_at, None,
        "another self-claim: the window starts over"
    );

    s.set_handover(Arc::new(AtomicBool::new(false)));
    s.set_approval_ledger(Some(PathBuf::from("/l/s-1.jsonl")));
    s.type_under_hand(true);
    s.set_reconnect(Duration::from_secs(3));
    s.set_claim_timing(Duration::from_millis(10), 99);
    let setup = s.setup().clone();
    assert!(setup.handover.0.is_some());
    assert_eq!(setup.ledger_path(), Some(Path::new("/l/s-1.jsonl")));
    assert!(setup.types_under_hand);
    assert_eq!(setup.reconnect, Duration::from_secs(3));
    assert_eq!(
        (setup.claim_renew, setup.claim_ttl_ms),
        (Duration::from_millis(10), 99)
    );

    let mut ctl2 = NoCtl;
    let rebuilt = Session::from_setup(&mut ctl2, setup.clone());
    assert_eq!(rebuilt.setup(), &setup);
}

#[test]
fn a_stand_in_workers_environment_is_never_written() {
    let planted = "sk-ant-planted-0123456789";
    let env = crate::supervise::policy::WorkerEnv::hermetic(None)
        .with(&format!("ANTHROPIC_API_KEY={planted}"));
    assert_eq!(
        env.var("ANTHROPIC_API_KEY"),
        Some(planted),
        "the negative control"
    );
    assert!(!format!("{env:?}").contains(planted), "{env:?}");
    let digest = env.digest(crate::supervise::tape::header::digest_seed("t-1"));
    let mut setup = SessionSetup::new(None);
    setup.approval_env.worker = WorkerSource::Fixed(Ok(env));
    let text =
        Header::capture("t-1", Entry::Watch, None, &setup, &SuperviseOpts::default()).encode();
    assert!(!text.contains(planted), "{text}");
    assert!(!text.contains("ANTHROPIC_API_KEY"), "{text}");
    assert!(
        text.lines()
            .any(|l| l == format!("worker=fixed {digest:016x} -")),
        "{text}"
    );
    let back = Header::decode(&text).expect("decodes");
    assert_eq!(
        back.setup.approval_env.worker,
        WorkerSource::Fixed(Ok(crate::supervise::policy::WorkerEnv::recorded(
            digest, None
        )))
    );
    setup.approval_env.worker = WorkerSource::Fixed(Err("no worker here".to_string()));
    let text =
        Header::capture("t-1", Entry::Watch, None, &setup, &SuperviseOpts::default()).encode();
    let back = Header::decode(&text).expect("decodes");
    assert_eq!(
        back.setup.approval_env.worker,
        WorkerSource::Fixed(Err("no worker here".to_string()))
    );
    let older = text.replace("worker=fixed-err no worker here", "worker=fixed");
    assert_ne!(older, text);
    let back = Header::decode(&older).expect("an older header decodes");
    assert_eq!(
        back.setup.approval_env.worker,
        WorkerSource::Fixed(Err(WORKER_NOT_TAPED.to_string()))
    );
}

#[test]
fn an_older_tape_waits_in_the_step_its_recorded_settle_gave() {
    let mut setup = SessionSetup::new(None);
    setup.approval_env.worker = WorkerSource::Session;
    setup.background_settle = Duration::from_secs(2);
    setup.wait_step = Duration::from_secs(2);
    let text = Header::capture("t", Entry::Watch, None, &setup, &SuperviseOpts::default()).encode();
    assert!(text.lines().any(|l| l == "wait_step=2s"), "{text}");
    let back = Header::decode(&text).expect("this build's tape");
    assert_eq!(back.setup, setup);
    assert!(back.supplied.is_empty(), "{:?}", back.supplied);
    let older: String = text
        .lines()
        .filter(|l| !l.starts_with("wait_step="))
        .map(|l| format!("{l}\n"))
        .collect();
    let back = Header::decode(&older).expect("an older build's tape");
    assert_eq!(back.setup, setup, "the step the recorded settle gave");
    assert_eq!(back.supplied, ["[setup] wait_step"]);
}

#[test]
fn the_ladder_seam_scales_the_short_and_outage_ladders_and_nothing_else() {
    use std::ffi::OsStr;
    let shipped = TurnEndTiming::default();
    let scaled = TurnEndTiming::default().ladder_scaled(0.125);
    let steps = |t: &TurnEndTiming| -> Vec<u64> {
        (0..7)
            .map(|n| {
                crate::supervise::ladder::doubling(t.short_backoff, n, t.short_backoff_max)
                    .as_secs()
            })
            .collect()
    };
    let outage = |t: &TurnEndTiming| -> Vec<u64> {
        (0..7)
            .map(|n| crate::supervise::ladder::doubling(t.outage_first, n, t.outage_cap).as_secs())
            .collect()
    };
    assert_eq!(steps(&scaled), [15, 30, 60, 120, 240, 450, 450]);
    assert_eq!(steps(&shipped), [120, 240, 480, 960, 1920, 3600, 3600]);
    assert_eq!(
        outage(&scaled),
        steps(&scaled),
        "the outage ladder scales as the short one"
    );
    assert_eq!(outage(&shipped), steps(&shipped));
    assert_eq!(
        TurnEndTiming {
            short_backoff: shipped.short_backoff,
            short_backoff_max: shipped.short_backoff_max,
            outage_first: shipped.outage_first,
            outage_cap: shipped.outage_cap,
            ..scaled.clone()
        },
        shipped,
        "only the ladders move"
    );
    assert_eq!(TurnEndTiming::default().ladder_scaled(1.0), shipped);
    let read = |v: &str| TurnEndTiming::ladder_scale(Some(OsStr::new(v)));
    assert_eq!(read("0.125"), Some(0.125));
    assert_eq!(read(" 0.5 "), Some(0.5));
    assert_eq!(read("1"), Some(1.0));
    for bad in ["0", "-0.5", "1.5", "nan", "inf", "eighth", ""] {
        assert_eq!(read(bad), None, "{bad:?}");
    }
    assert_eq!(TurnEndTiming::ladder_scale(None), None);
    let mut setup = SessionSetup::new(None);
    setup.approval_env.worker = WorkerSource::Session;
    setup.turn_end_timing = scaled.clone();
    let text = Header::capture("t", Entry::Watch, None, &setup, &SuperviseOpts::default()).encode();
    assert!(
        text.lines().any(|l| l == "turn_end.short_backoff=15s"),
        "{text}"
    );
    let back = Header::decode(&text).expect("decodes");
    assert_eq!(back.setup.turn_end_timing, scaled);
}

/// THE RESET SEAM SCALES THE WAIT PAST A WALL'S RESET AND NOTHING ELSE, beside the ladder's and
/// the span's: a world test's carry-on comes at the wall's own reset (`ATERM_DEV_RESET_SCALE`).
#[test]
fn the_reset_seam_scales_the_wait_past_a_walls_reset_and_nothing_else() {
    use crate::supervise::policy::turn_end::{
        LADDER_SCALE_SEAM, RESET_SCALE_SEAM, RESET_SPAN_SCALE_SEAM,
    };
    use std::ffi::OsStr;
    let shipped = TurnEndTiming::default();
    let scaled = TurnEndTiming::for_seams(None, Some(OsStr::new("0.5")), None);
    assert_eq!(
        (scaled.reset_grace, scaled.reset_jitter),
        (Duration::from_secs(75), Duration::from_secs(30))
    );
    assert_eq!(
        TurnEndTiming {
            reset_grace: shipped.reset_grace,
            reset_jitter: shipped.reset_jitter,
            ..scaled.clone()
        },
        shipped,
        "only the reset's wait moves"
    );
    let both = TurnEndTiming::for_seams(Some(OsStr::new("0.125")), Some(OsStr::new("0.5")), None);
    assert_eq!(
        both,
        scaled.clone().ladder_scaled(0.125),
        "each seam its own"
    );
    let all = TurnEndTiming::for_seams(
        Some(OsStr::new("0.125")),
        Some(OsStr::new("0.5")),
        Some(OsStr::new("0.05")),
    );
    assert_eq!(all, both.reset_span_scaled(0.05), "each seam its own");
    assert_eq!(TurnEndTiming::for_seams(None, None, None), shipped);
    for bad in ["0", "1.5", "nan", "soon"] {
        let bad = Some(OsStr::new(bad));
        assert_eq!(
            TurnEndTiming::for_seams(None, bad, None),
            shipped,
            "{bad:?}"
        );
        assert_eq!(
            TurnEndTiming::for_seams(None, None, bad),
            shipped,
            "{bad:?}"
        );
    }
    assert_ne!(LADDER_SCALE_SEAM, RESET_SCALE_SEAM);
    assert_ne!(RESET_SPAN_SCALE_SEAM, RESET_SCALE_SEAM);
    assert_ne!(RESET_SPAN_SCALE_SEAM, LADDER_SCALE_SEAM);
}

/// THE SPAN SEAM PLACES A WALL'S RESET SOONER, NEVER BEFORE ITS SCALED TIME
/// (`ATERM_DEV_RESET_SPAN_SCALE`): at a twentieth, `in 1m` falls three seconds after a read on a
/// whole second, and on the first whole second at or after its scaled time from any other read;
/// a shipped timing, the seam at one, a reset at or before the read and one too far off to scale
/// leave it where its words name it.
#[test]
fn the_span_seam_places_a_walls_reset_sooner_and_never_before_its_scaled_time() {
    use std::ffi::OsStr;
    let shipped = TurnEndTiming::default();
    let span = TurnEndTiming::for_seams(None, None, Some(OsStr::new("0.05")));
    assert_eq!(span.reset_minute, Duration::from_secs(3));
    assert_eq!(
        TurnEndTiming {
            reset_minute: shipped.reset_minute,
            ..span.clone()
        },
        shipped,
        "only the time to the reset moves"
    );
    assert!(shipped.reset_as_named() && !span.reset_as_named());
    let read_s: i64 = 1_793_527_588;
    assert_eq!(span.reset_placed(read_s + 60, read_s * 1000), read_s + 3);
    // Read 700 ms into its second, `in 1m` is 59.3 s off: 2.965 s at a twentieth, placed on the
    // next whole second.
    let read_ms = read_s * 1000 + 700;
    assert_eq!(span.reset_placed(read_s + 60, read_ms), read_s + 4);
    for frac in [0, 1, 299, 500, 999] {
        for minutes in [1, 2, 5, 60] {
            let now_ms = read_s * 1000 + frac;
            let at = read_s + minutes * 60;
            let placed = span.reset_placed(at, now_ms);
            // Twenty times the scaled time, in milliseconds: the read's and the span's.
            let scaled = now_ms * 20 + (at * 1000 - now_ms);
            let after = placed * 1000 * 20 - scaled;
            assert!(
                (0..20 * 1000).contains(&after),
                "{minutes}m read {frac} ms into its second: placed {placed}, {after} past"
            );
            assert!(placed <= at, "{minutes}m: placed after its words' reset");
        }
    }
    assert_eq!(span.reset_placed(read_s, read_ms), read_s, "passed");
    assert_eq!(span.reset_placed(read_s - 60, read_ms), read_s - 60);
    assert_eq!(shipped.reset_placed(read_s + 60, read_ms), read_s + 60);
    assert_eq!(
        TurnEndTiming::for_seams(None, None, Some(OsStr::new("1"))),
        shipped
    );
    let unbounded = TurnEndTiming {
        reset_minute: Duration::MAX,
        ..shipped.clone()
    };
    assert_eq!(unbounded.reset_placed(read_s + 60, read_ms), read_s + 60);
}
