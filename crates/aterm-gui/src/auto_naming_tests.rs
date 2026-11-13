// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_vitals::jobs::{NamedRow, TierB};

/// An agent's call quiet in its suspect episode `episode`, waiting on what no kernel read names.
fn quiet(id: &str, episode: u32) -> aterm_vitals::jobs::JobRow {
    aterm_vitals::jobs::JobRow {
        kind: JobKind::AgentCall,
        class: Class::Waiting,
        standing: Some(Standing::Capped),
        episode,
        ..crate::stats_paint::tests::job_row(id, Verdict::Quiet, Some(3_000), Some(600_000))
    }
}

fn view(rows: Vec<aterm_vitals::jobs::JobRow>) -> JobsView {
    JobsView {
        rows,
        ..JobsView::default()
    }
}

/// A quiet agent call is sampled once an episode, and again in the next episode progress opens.
/// CONTROLS: a call a name holds, a person's job, a call at work and a legitimate wait are never
/// sampled; a job gone from the view is forgotten, so its id is sampled afresh.
#[test]
fn a_quiet_call_is_sampled_once_an_episode() {
    let tried = Tried::new();
    let first = view(vec![quiet("a1", 1)]);
    assert_eq!(tried.next_due(&first), Some((7, "a1".to_string())));
    assert_eq!(tried.next_due(&first), None, "once an episode");
    assert_eq!(
        tried.next_due(&view(vec![quiet("a1", 2)])),
        Some((7, "a1".to_string())),
        "a new episode is sampled again"
    );
    let mut named = quiet("a2", 1);
    named.named = Some(NamedRow {
        word: TierB::Timer,
        holder: None,
    });
    let mut person = quiet("a3", 1);
    person.kind = JobKind::Shell;
    let mut working = quiet("a4", 1);
    working.class = Class::Running;
    let mut legit = quiet("a5", 1);
    legit.standing = Some(Standing::Legit);
    let mut fine = quiet("a6", 1);
    fine.verdict = Verdict::Fine;
    let controls = view(vec![quiet("a1", 2), named, person, working, legit, fine]);
    assert_eq!(tried.next_due(&controls), None);
    tried.next_due(&view(Vec::new()));
    assert_eq!(
        tried.next_due(&first),
        Some((7, "a1".to_string())),
        "a job gone and back is new"
    );
}
