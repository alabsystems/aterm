// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`SessionSetup`]: what the setters write into it, and that a tape's
//! header gives every field of it back.

use super::*;
use crate::supervise::tape::header::{Entry, Header};

/// A transport no setup test may use: building a session and reading its
/// setup sends nothing.
struct NoCtl;

impl Ctl for NoCtl {
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        panic!("a setup test sent {args:?}");
    }
}

/// Every field away from [`SessionSetup::new`]'s value, each duration odd to
/// the nanosecond (none is a shipped value, and each takes the header's
/// finest unit), the worker read from the session (a stand-in's environment
/// is never written, so it does not come back), and the ledger under a root.
/// A whole literal: a field added to the setup does not build here until it
/// is given a value of its own.
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
        box_call_step: d(56),
        stuck_after: d(50),
        person_hold_wait: Some(d(52)),
        beside_idle_min: d(53),
    }
}

/// The `[setup]` and `[env]` lines of a header's text.
fn setup_lines(text: &str) -> Vec<&str> {
    text.lines()
        .skip_while(|l| *l != "[setup]")
        .take_while(|l| *l != "[opts]")
        .collect()
}

/// THE SETUP ROUND TRIP: a setup with every field moved comes back from a
/// tape's header field for field — and every one of its lines differs from
/// the shipped setup's (sections aside), so no field rides through on a
/// default the two share. A raised hand-over flag comes back as a flag
/// present and not raised: its value is the host's to raise, never setup.
#[test]
fn every_field_of_the_setup_comes_back_from_the_header() {
    // An absolute root of this host (a `/`-rooted path names no drive on Windows).
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

/// [`Session::new`] is [`Session::from_setup`] over the shipped setup, and
/// each setter writes the setup a header then reads: the holder is resolved
/// as it is set (`aterm-supervise:<pid>` unnamed), the turn-end timings are
/// both the setup's and the policy's fresh memory, a self-claim moved
/// forgets the remedy's window.
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

    // The setup alone rebuilds the same session's setup.
    let mut ctl2 = NoCtl;
    let rebuilt = Session::from_setup(&mut ctl2, setup.clone());
    assert_eq!(rebuilt.setup(), &setup);
}

/// A stand-in worker environment is written as its kind and the digest the
/// tape names it by (seeded with the tape's own id), never its variables, and
/// comes back as a worker known only by that digest: a planted secret is
/// nowhere in the header (and the negative control — the variable itself,
/// which a careless encoder would write — holds it, so the check can fail).
/// The environment's own `Debug` names no variable either (it counts them),
/// so not even a `Debug` of a setup can carry one. A stand-in that is an
/// error comes back with its words; a bare `fixed` (a header from before the
/// digest) as one that says it was never written.
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
