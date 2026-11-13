// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    AgentPrimeOutcome, PrimeCue, PrimeSchedule, agent_prime_pass, agent_prime_permitted,
    run_agent_prime, take_pass,
};
use std::time::{Duration, Instant};

#[test]
fn an_uncommitted_candidate_writes_no_agents_file() {
    if !crate::test_alone::alone() {
        return;
    }
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let home = scratch.path();
    std::fs::create_dir_all(home.join(".codex")).expect("detect codex");
    let prime = || {
        crate::test_env::scoped_unset("XDG_CONFIG_HOME", || {
            run_agent_prime(true, false, Some(home.to_path_buf()))
        })
    };
    assert_eq!(agent_prime_pass(|| false, prime), None);
    assert!(!home.join(".codex/AGENTS.md").exists());
    assert!(matches!(
        agent_prime_pass(|| true, prime),
        Some(AgentPrimeOutcome::Ran(_))
    ));
    assert!(home.join(".codex/AGENTS.md").is_file());
}

fn passes(cues: &[(PrimeCue, u64)]) -> Vec<usize> {
    let t0 = Instant::now();
    let mut last = None;
    let mut started = Vec::new();
    for (i, &(cue, s)) in cues.iter().enumerate() {
        if take_pass(cue, &mut last, t0 + Duration::from_secs(s)) {
            started.push(i);
        }
    }
    started
}

#[test]
fn the_launch_primes_the_tabs_an_update_hands_over() {
    let adopted = PrimeCue::Spawn {
        adopted: true,
        spare: false,
    };
    let fresh = PrimeCue::Spawn {
        adopted: false,
        spare: false,
    };
    let spare = PrimeCue::Spawn {
        adopted: false,
        spare: true,
    };
    let successor = [
        (PrimeCue::Launch, 0),
        (adopted, 0),
        (adopted, 1),
        (spare, 2),
    ];
    assert_eq!(passes(&successor), [0], "the launch's pass, and only it");
    assert_eq!(
        passes(&successor[1..]),
        [0_usize; 0],
        "adoptions alone prime nothing"
    );
    assert_eq!(passes(&[(PrimeCue::Launch, 0), (fresh, 0)]), [0]);
    let later = [
        (PrimeCue::Launch, 0),
        (spare, 61),
        (PrimeCue::SpareAdopted, 62),
        (adopted, 200),
        (fresh, 200),
    ];
    assert_eq!(passes(&later), [0, 2, 4]);
}

fn started(steps: &[Option<(PrimeCue, u64)>], held: bool) -> Vec<(usize, PrimeCue)> {
    let t0 = Instant::now();
    let mut schedule = PrimeSchedule::AT_LAUNCH;
    if !held {
        assert_eq!(schedule.release(), None);
    }
    let mut out = Vec::new();
    for (i, step) in steps.iter().enumerate() {
        let start = match *step {
            Some((cue, s)) => schedule.admit(cue, t0 + Duration::from_secs(s)),
            None => schedule.release(),
        };
        out.extend(start.map(|cue| (i, cue)));
    }
    out
}

#[test]
fn no_pass_starts_before_the_launchs_first_frame() {
    let fresh = PrimeCue::Spawn {
        adopted: false,
        spare: false,
    };
    let cold = [
        Some((PrimeCue::Launch, 0)),
        Some((fresh, 0)),
        None,
        None,
        Some((PrimeCue::SpareAdopted, 61)),
    ];
    assert_eq!(
        started(&cold, true),
        [(2, PrimeCue::Launch), (4, PrimeCue::SpareAdopted)]
    );
    assert_eq!(
        started(
            &[Some((PrimeCue::Launch, 0)), Some((fresh, 61)), None],
            true
        ),
        [(2, PrimeCue::Launch)]
    );
    assert_eq!(started(&[Some((fresh, 0)), None], true), [(1, fresh)]);
    assert_eq!(started(&[None, Some((fresh, 0))], true), [(1, fresh)]);
    assert_eq!(started(&[None, None], true), []);
    assert_eq!(
        started(&cold, false),
        [(0, PrimeCue::Launch), (4, PrimeCue::SpareAdopted)],
        "the control: unheld, the launch's pass starts at its cue"
    );
}

#[test]
fn a_headless_instance_never_primes() {
    assert!(!agent_prime_permitted(false, true), "headless instance");
    assert!(!agent_prime_permitted(true, false), "unit test");
    assert!(!agent_prime_permitted(true, true));
    assert!(
        agent_prime_permitted(false, false),
        "a windowed aterm primes"
    );
}

#[test]
fn a_pass_reports_exactly_what_it_did() {
    if !crate::test_alone::alone() {
        return;
    }
    assert_eq!(
        run_agent_prime(false, false, None),
        AgentPrimeOutcome::Disabled
    );
    assert_eq!(
        run_agent_prime(true, false, None),
        AgentPrimeOutcome::NoHome
    );

    let home = aterm_tempfile::tempdir().expect("scratch home");
    std::fs::create_dir_all(home.path().join(".codex")).expect("detect codex");
    let AgentPrimeOutcome::Ran(pass) = crate::test_env::scoped_unset("XDG_CONFIG_HOME", || {
        run_agent_prime(true, false, Some(home.path().to_path_buf()))
    }) else {
        panic!("enabled with a home must run");
    };
    assert!(pass.changed(), "{}", pass.summary);
    assert_eq!(pass.agents.len(), 1);
    assert_eq!(pass.agents[0].agent, "codex");
    assert_eq!(pass.agents[0].outcome, aterm_primer::Outcome::Installed);
    let written =
        std::fs::read_to_string(home.path().join(".codex/AGENTS.md")).expect("the primer landed");
    assert_eq!(written, aterm_primer::primer_block(Some("codex")));
    assert!(
        !home.path().join(".claude").exists(),
        "never creates an agent dir"
    );

    let dev_home = aterm_tempfile::tempdir().expect("scratch home");
    std::fs::create_dir_all(dev_home.path().join(".codex")).expect("detect codex");
    assert_eq!(
        run_agent_prime(true, true, Some(dev_home.path().to_path_buf())),
        AgentPrimeOutcome::NotReleaseBundle
    );
    assert!(
        !dev_home.path().join(".codex/AGENTS.md").exists(),
        "nothing is written from a non-release bundle"
    );
}
