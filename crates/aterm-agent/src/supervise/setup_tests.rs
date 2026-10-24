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
            down_hold: d(11),
            progress: d(12),
            reset_grace: d(13),
            reset_jitter: d(14),
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
    let ours = setup_lines(&text);
    let theirs_text = Header::capture("t-1", Entry::Watch, Some(dir), &shipped, &opts).encode();
    let theirs = setup_lines(&theirs_text);
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
fn the_ladder_seam_scales_the_short_back_off_ladder_and_nothing_else() {
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
    assert_eq!(steps(&scaled), [15, 30, 60, 120, 240, 450, 450]);
    assert_eq!(steps(&shipped), [120, 240, 480, 960, 1920, 3600, 3600]);
    assert_eq!(
        TurnEndTiming {
            short_backoff: shipped.short_backoff,
            short_backoff_max: shipped.short_backoff_max,
            ..scaled.clone()
        },
        shipped,
        "only the ladder moves"
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
