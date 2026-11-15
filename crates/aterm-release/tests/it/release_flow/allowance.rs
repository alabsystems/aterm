// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A step that ended only on agh's `rate_limited` refusal waits for GitHub's allowance and asks
//! again, once (`Run::past_allowance` in [`crate::release`]), against the fake world
//! ([`super::World`]).

use super::{Ran, World, bump};
use crate::release::Refusal;

/// What agh's command ends a refused read with, as the engine's log carries it: 0.200.0's step
/// 7, 2026-11-13.
fn agh_refusal(reset: u64) -> String {
    format!(
        "FAIL: cannot query publication engine origin/main: pub GitHub transport: Agh exited \
         11: agh: rate_limited exit=11 verb=api repo=fixture-dev/publication as=dev \
         sub=pre_refused reset={reset} side_effect=none why=\"background core read leaves \
         1000 requests for the release cutter until {reset}\""
    )
}

/// A `pub stage` that ended on that refusal.
fn limited(reset: u64) -> Ran {
    Ran {
        code: 1,
        detail: vec![
            "pushed to staging (append-only snapshot)".into(),
            agh_refusal(reset),
        ],
    }
}

/// What this tool's own client says of a spent allowance (`github_remote::words`), in a
/// refusal that decided nothing.
fn spent(reset: u64) -> Refusal {
    Refusal::new(
        format!(
            "could not read the engine's origin (the rate book says core is spent for this \
             identity until {reset} (agh: rate_limited exit=11 reset={reset}))"
        ),
        "run it again once the channel answers",
    )
    .could_not_run()
}

fn count(w: &World, call: &str) -> usize {
    w.calls.iter().filter(|c| c.as_str() == call).count()
}

fn waits(w: &World) -> Vec<&String> {
    w.out
        .iter()
        .filter(|line| line.contains("for GitHub's allowance"))
        .collect()
}

fn slept(w: &World) -> u64 {
    w.calls
        .iter()
        .filter_map(|c| c.strip_prefix("sleep ")?.parse::<u64>().ok())
        .sum()
}

/// R32. The engine moved after verify, so step 7 stages R again, and that stage's engine ends
/// on agh's refusal with a reset 36 minutes ahead. The run says so in one line, waits past the
/// reset with its lease renewed every five minutes, stages again and publishes.
#[test]
fn a_stage_refused_for_the_allowance_waits_for_its_reset_and_runs_again() {
    let mut w = World::new();
    w.after_verify = Some(|w| {
        w.engine = "f3f3f3f3f3".into();
        w.stage_answers.push_back(limited(w.now + 2165));
    });
    let (code, last) = w.run(bump());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert_eq!(
        count(&w, "stage"),
        3,
        "step 4's, the refused one, and again: {:?}",
        w.calls
    );
    let said = waits(&w);
    assert_eq!(said.len(), 1, "{:#?}", w.out);
    assert!(
        said[0].contains("publish")
            && said[0].contains("waiting until ")
            && said[0].contains(" for GitHub's allowance"),
        "{}",
        said[0]
    );
    let slept = slept(&w);
    assert!(
        (1900..=2180).contains(&slept),
        "it waits past the reset and no longer: {slept} s, {:?}",
        w.calls
    );
    let renewals = w
        .calls
        .iter()
        .filter(|c| c.starts_with("fleet-wait "))
        .count();
    assert!(
        renewals >= 2165 / 300,
        "the lease is renewed every five minutes of the wait: {renewals}, {:?}",
        w.calls
    );
    assert!(
        w.calls.iter().all(|c| c
            .strip_prefix("sleep ")
            .is_none_or(|secs| secs.parse::<u64>().unwrap() <= 300)),
        "no sleep outlasts a renewal: {:?}",
        w.calls
    );
}

/// NEGATIVE CONTROLS. A second refusal for the allowance in the same step ends the run as it
/// did (one wait, no third try); a reset more than an hour ahead, and a stage that failed for
/// another reason, are not waited on at all.
#[test]
fn a_second_refusal_a_distant_reset_and_another_failure_end_the_run_as_before() {
    let refused = |answers: fn(&mut World)| {
        let mut w = World::new();
        w.after_verify = Some(answers);
        let (code, last) = w.run(bump());
        (w, code, last)
    };

    let (w, code, last) = refused(|w| {
        w.engine = "f3f3f3f3f3".into();
        w.stage_answers.push_back(limited(w.now + 600));
        w.stage_answers.push_back(limited(w.now + 600 + 3600));
    });
    assert_eq!(code, 1, "{last}");
    assert!(
        last.contains("pub stage did not stage") && last.contains("nothing public"),
        "{last}"
    );
    assert_eq!(waits(&w).len(), 1, "one wait: {:#?}", w.out);
    assert_eq!(
        count(&w, "stage"),
        3,
        "no third try in the step: {:?}",
        w.calls
    );

    let (w, code, last) = refused(|w| {
        w.engine = "f3f3f3f3f3".into();
        w.stage_answers.push_back(limited(w.now + 3600 + 600));
    });
    assert_eq!(code, 1, "{last}");
    assert!(waits(&w).is_empty() && slept(&w) == 0, "{:#?}", w.out);
    assert_eq!(count(&w, "stage"), 2, "{:?}", w.calls);

    let (w, code, last) = refused(|w| {
        w.engine = "f3f3f3f3f3".into();
        w.stage_answers.push_back(Ran {
            code: 1,
            detail: vec!["FAIL: forbidden content in README.md".into()],
        });
    });
    assert_eq!(code, 1, "{last}");
    assert!(waits(&w).is_empty() && slept(&w) == 0, "{:#?}", w.out);
    assert_eq!(count(&w, "stage"), 2, "{:?}", w.calls);
}

/// The release's own client: step 7's preparation refused with agh's words for a spent
/// allowance waits and asks again. NEGATIVE CONTROL: refused twice, the run ends COULD NOT RUN
/// after one wait.
#[test]
fn the_runs_own_client_refused_for_the_allowance_waits_once() {
    let mut w = World::new();
    w.after_verify = Some(|w| w.publish_begin_refusals.push_back(spent(w.now + 900)));
    let (code, last) = w.run(bump());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert_eq!(waits(&w).len(), 1, "{:#?}", w.out);
    assert!((700..=915).contains(&slept(&w)), "{:?}", w.calls);

    let mut w = World::new();
    w.after_verify = Some(|w| {
        w.publish_begin_refusals.push_back(spent(w.now + 900));
        w.publish_begin_refusals
            .push_back(spent(w.now + 900 + 3600));
    });
    let (code, last) = w.run(bump());
    assert_eq!(code, 3, "{last}");
    assert!(last.contains("COULD NOT RUN"), "{last}");
    assert_eq!(waits(&w).len(), 1, "{:#?}", w.out);
}

/// The wait renews the lease, and a line another holder took during it ends the run there: the
/// renewal is the boundary's own check, not a timer.
#[test]
fn a_line_lost_during_the_wait_ends_the_run() {
    let mut w = World::new();
    w.after_verify = Some(|w| {
        w.engine = "f3f3f3f3f3".into();
        w.stage_answers.push_back(limited(w.now + 1200));
        w.fleet_lost_in_sleep = true;
    });
    let (code, last) = w.run(bump());
    assert_eq!(code, 3, "{last}");
    assert!(last.contains("fleet line was adopted"), "{last}");
    assert_eq!(
        count(&w, "stage"),
        2,
        "no stage after the line was lost: {:?}",
        w.calls
    );
}
