// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The renewal connection drive's cadence against a host whose successor never attaches.

use super::*;
use crate::harness::codex_renewal::{self, Backoff, Pace};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// A clock that moves only when the drive sleeps, so a whole 120 s budget runs at once.
struct Virtual(Arc<AtomicU64>);

impl Pace for Virtual {
    fn now_us(&self) -> Option<u64> {
        Some(self.0.load(Ordering::Acquire))
    }

    fn sleep(&self, wait: std::time::Duration) {
        let wait = u64::try_from(wait.as_micros()).unwrap();
        self.0.fetch_add(wait, Ordering::AcqRel);
    }
}

/// A bound renewal record whose agent has exited: its pid now names a process born at another
/// time, as the kernel reports once the renewal's Signal ended it.
fn exited_record(opts: &Opts) -> (St, Prepare) {
    let mut st = bound_record(opts);
    let Request::Prepare(mut prepare) = Request::parse(&st.native_prepare).unwrap() else {
        panic!("saved Prepare")
    };
    let NativePlan::Restart { process, .. } = &mut prepare.plan else {
        panic!("a restart plan")
    };
    process.birth += 1;
    prepare.expected_human_seq.get_or_insert(0);
    st.native_prepare = Request::Prepare(prepare.clone()).encode().unwrap();
    (st, *prepare)
}

/// What the host was asked, each at the drive's clock.
#[derive(Default)]
struct Asked {
    queries: Vec<u64>,
    resumes: Vec<(ImagePhase, u64)>,
}

/// How the fake host answers each Resume.
#[derive(Clone, Copy, Default)]
struct Answers {
    /// Every Resume is answered `state=unknown`, a typed one included.
    unknown: bool,
    /// How many Rollback Resumes are answered `state=unknown` before one is typed, as the
    /// host answers while the forward image's group has not settled.
    rollback_held: usize,
    /// The first Query after this phase's first Resume reports its launch unissued, and the
    /// host may type it again, as once a launcher that never ran is gone.
    unissued: Option<ImagePhase>,
}

/// A host whose successor never attaches, answering as `native_act` does once its launcher was
/// refused before it registered: a phase's first Resume types its line (`state=resumed`), each
/// repeat is answered `state=unknown`, and every Query reads `state=unknown`.
fn never_attached(
    prepare: &Prepare,
    clock: &Arc<AtomicU64>,
    asked: &Arc<Mutex<Asked>>,
    answers: Answers,
) -> impl FnMut(&str) -> Option<String> + Send + 'static {
    let id = prepare.id.clone();
    let clock = Arc::clone(clock);
    let asked = Arc::clone(asked);
    let mut typed = [false; 2];
    let mut rollback_held = answers.rollback_held;
    let mut proved = answers.unissued.is_none();
    let index = |phase| usize::from(phase == ImagePhase::Rollback);
    move |line| {
        let now = clock.load(Ordering::Acquire);
        if let Some(reply) = common(line) {
            return Some(reply);
        }
        if line == "AUTH private-memory-fixture-token" {
            return Some(String::new());
        }
        if line == "version" {
            return Some("OK 0\n".into());
        }
        if line == format!("@{SID} text --json") {
            return Some(
                "OK {\"rows\":[\"% \"],\"cursor\":{\"row\":0,\"col\":2},\"seq\":5,\"gen\":\"1.5\",\"human_ms\":null,\"first\":0}\n"
                    .into(),
            );
        }
        Some(match request(line) {
            Request::Prepare(plan) => {
                assert_eq!(plan.id, id);
                format!(
                    "OK native-act state=admitted id={} cap={}\n",
                    plan.id,
                    binding().capability
                )
            }
            Request::Signal(_) => "OK native-act state=signalled\n".into(),
            Request::Query(_) => {
                let mut asked = asked.lock().unwrap();
                asked.queries.push(now);
                match answers.unissued {
                    Some(phase) if !proved && typed[index(phase)] => {
                        proved = true;
                        typed[index(phase)] = false;
                        format!("OK native-act state=unissued phase={}\n", phase.word())
                    }
                    _ => "OK native-act state=unknown\n".into(),
                }
            }
            Request::Precommit(..) | Request::Commit(..) => "OK native-act state=unknown\n".into(),
            Request::Resume(_, phase, _, _) => {
                asked.lock().unwrap().resumes.push((phase, now));
                if phase == ImagePhase::Rollback && rollback_held > 0 {
                    rollback_held -= 1;
                    "OK native-act state=unknown\n".into()
                } else if answers.unknown || typed[index(phase)] {
                    "OK native-act state=unknown\n".into()
                } else {
                    typed[index(phase)] = true;
                    format!("OK native-act state=resumed phase={}\n", phase.word())
                }
            }
            other => panic!("unexpected renewal request {other:?}"),
        })
    }
}

/// Run one whole 120 s root budget, Signal first, on a virtual clock against `answers`.
fn root_budget(answers: Answers) -> (Asked, u64) {
    let world = aterm_tempfile::tempdir().unwrap();
    let opts = opts(world.path());
    let (st, prepare) = exited_record(&opts);
    let start = prepare.deadline_us - 120_000_000;
    let clock = Arc::new(AtomicU64::new(start));
    let asked = Arc::new(Mutex::new(Asked::default()));
    let (client, worker) = endpoint(never_attached(&prepare, &clock, &asked, answers));
    assert!(
        codex_renewal::drive_connection_paced(
            &opts,
            SID,
            &prepare,
            st,
            client,
            true,
            &Virtual(Arc::clone(&clock))
        )
        .is_none()
    );
    worker.join().unwrap();
    let asked = std::mem::take(&mut *asked.lock().unwrap());
    (asked, start)
}

fn micros(wait: std::time::Duration) -> u64 {
    u64::try_from(wait.as_micros()).unwrap()
}

fn phases(asked: &Asked) -> Vec<ImagePhase> {
    asked.resumes.iter().map(|(phase, _)| *phase).collect()
}

/// The finding's storm: a root whose launcher was refused sent Resume on every 100 ms poll
/// for its whole 120 s budget. Once the host typed a phase's line it is not asked again, and
/// the Query polls back off from 100 ms to a 2 s cap while the host answers nothing new, the
/// Rollback the host types resetting them once.
#[test]
fn a_never_attached_root_asks_each_typed_phase_once_and_backs_off_its_polls() {
    let (asked, start) = root_budget(Answers::default());
    assert_eq!(phases(&asked), [ImagePhase::Forward, ImagePhase::Rollback]);
    assert!(asked.resumes[1].1 >= start + 60_000_000);
    let (floor, cap) = (micros(Backoff::FLOOR), micros(Backoff::CAP));
    let gaps = asked
        .queries
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .collect::<Vec<_>>();
    assert_eq!(
        gaps[..6],
        [floor, 2 * floor, 4 * floor, 8 * floor, 16 * floor, cap]
    );
    assert!(gaps.iter().all(|gap| *gap <= cap));
    let rollback = asked
        .queries
        .iter()
        .position(|at| *at > asked.resumes[1].1)
        .unwrap();
    assert_eq!(
        gaps[rollback - 1],
        floor,
        "the Rollback the host typed moved the drive"
    );
    // About 34 polls a phase (the old drive made 1200 polls and 1200 Resumes in one budget).
    assert!(
        asked.queries.len() <= 70,
        "{} polls in one budget",
        asked.queries.len()
    );
}

/// The host answers a Rollback `state=unknown` while the crashed forward image's group lingers,
/// and its Query reads `state=unknown` before and after that group is gone. The held Rollback is
/// asked again on the next poll, at the backoff pace, until the host types it; then never again.
#[test]
fn a_rollback_held_while_the_forward_image_settles_is_asked_again_until_typed() {
    let (asked, start) = root_budget(Answers {
        rollback_held: 3,
        ..Answers::default()
    });
    assert_eq!(
        phases(&asked),
        [
            ImagePhase::Forward,
            ImagePhase::Rollback,
            ImagePhase::Rollback,
            ImagePhase::Rollback,
            ImagePhase::Rollback
        ],
        "three held Rollbacks, then the typed one, then nothing more"
    );
    let cap = micros(Backoff::CAP);
    let rollbacks = asked.resumes[1..]
        .iter()
        .map(|(_, at)| *at)
        .collect::<Vec<_>>();
    assert!(rollbacks[0] >= start + 60_000_000);
    assert!(
        rollbacks.windows(2).all(|pair| pair[1] - pair[0] <= cap),
        "each held Rollback is asked again within one capped poll: {rollbacks:?}"
    );
}

/// A host that answers every Resume `state=unknown` (one an earlier drive already typed) is
/// asked once per poll, so the backoff bounds its Resumes as it bounds its Queries.
#[test]
fn a_host_that_answers_every_resume_unknown_is_asked_only_at_the_poll_pace() {
    let (asked, _) = root_budget(Answers {
        unknown: true,
        ..Answers::default()
    });
    assert_eq!(asked.resumes.len(), asked.queries.len());
    assert!(
        asked.resumes.len() <= 70,
        "{} Resumes in one budget (the old drive made 1200)",
        asked.resumes.len()
    );
    assert!(phases(&asked).contains(&ImagePhase::Forward));
    assert!(phases(&asked).contains(&ImagePhase::Rollback));
}

/// A typed phase is asked again only once the host proves its launch unissued: the same
/// Forward is typed once more, at once, and the drive then asks for nothing until Rollback.
#[test]
fn a_typed_phase_is_asked_again_only_when_the_host_reports_its_launch_unissued() {
    let (asked, start) = root_budget(Answers {
        unissued: Some(ImagePhase::Forward),
        ..Answers::default()
    });
    let resumes = asked
        .resumes
        .iter()
        .map(|(phase, at)| (*phase, at - start))
        .collect::<Vec<_>>();
    let floor = micros(Backoff::FLOOR);
    assert_eq!(
        resumes,
        [
            (ImagePhase::Forward, 0),
            (ImagePhase::Forward, floor),
            (ImagePhase::Rollback, resumes[2].1)
        ]
    );
}

/// A continuation the host latched in Rollback (its predecessor closed there) starts from
/// that phase: it asks for Rollback once and never for the Forward the host would refuse.
#[test]
fn a_rollback_latched_continuation_never_asks_for_forward() {
    let world = aterm_tempfile::tempdir().unwrap();
    let opts = opts(world.path());
    let (st, prepare) = exited_record(&opts);
    let clock = Arc::new(AtomicU64::new(prepare.deadline_us - 120_000_000));
    let asked = Arc::new(Mutex::new(Asked::default()));
    let (client, worker) = endpoint(never_attached(&prepare, &clock, &asked, Answers::default()));
    assert!(
        codex_renewal::drive_recovered(
            &opts,
            SID,
            &prepare,
            st,
            client,
            false,
            Some(ImagePhase::Rollback),
            &Virtual(Arc::clone(&clock))
        )
        .is_none()
    );
    let seen = worker.join().unwrap();
    assert!(!seen.iter().any(|line| line.contains(" native-act signal ")));
    assert_eq!(phases(&asked.lock().unwrap()), [ImagePhase::Rollback]);
}

/// The service's continuation entry ([`codex_renewal::drive_resume_only`]) hands the drive the
/// phase its host latched: over the host's own socket, on this host's clock, a continuation
/// latched in Rollback asks for Rollback and never for Forward or Signal.
#[test]
fn a_resume_only_drive_starts_in_the_phase_its_host_latched() {
    let world = aterm_tempfile::tempdir().unwrap();
    let opts = opts(world.path());
    let (mut st, mut prepare) = exited_record(&opts);
    // A short budget on this host's clock: the drive's own deadline ends it.
    prepare.deadline_us = native_act::monotonic_us().unwrap() + 1_500_000;
    st.native_prepare = Request::Prepare(Box::new(prepare.clone()))
        .encode()
        .unwrap();
    upgrade_drive::save_checked(&opts, &format!("renewal-{}", prepare.id), &st).unwrap();
    let clock = Arc::new(AtomicU64::new(0));
    let asked = Arc::new(Mutex::new(Asked::default()));
    let mut answer = never_attached(&prepare, &clock, &asked, Answers::default());
    let listener = std::os::unix::net::UnixListener::bind(opts.sock.as_deref().unwrap()).unwrap();
    let host = std::thread::spawn(move || {
        let (stream, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Until(std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN),
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        stream
            .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .unwrap();
        let mut writer = stream.try_clone().unwrap();
        let mut reader = std::io::BufReader::new(stream);
        let mut seen = Vec::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 {
                return seen;
            }
            let line = line.trim_end_matches(['\r', '\n']).to_string();
            seen.push(line.clone());
            writer.write_all(answer(&line).unwrap().as_bytes()).unwrap();
        }
    });
    assert!(codex_renewal::drive_resume_only(&opts, SID, &prepare, ImagePhase::Rollback).is_none());
    let seen = host.join().unwrap();
    assert!(!seen.iter().any(|line| line.contains(" native-act signal ")));
    assert_eq!(phases(&asked.lock().unwrap()), [ImagePhase::Rollback]);
}
