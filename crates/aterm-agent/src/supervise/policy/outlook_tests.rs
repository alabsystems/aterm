// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The outlook of a wall: a person is asked for only what only a person can
//! do, and a wait is never one.

use super::*;

fn cfg() -> SupervisorConfig {
    SupervisorConfig::default()
}

/// A bucket with no reset asks a person, one with a reset does not; a lost
/// login is a person's; a switched-off key is nobody's.
#[test]
fn every_wall_kind_has_an_outlook() {
    let c = cfg();
    let bucket = WallKind::ModelBucket { consent: false };
    assert_eq!(
        outlook(WallKind::UsageWeekly, true, false, &c, true),
        Outlook::Resumes
    );
    assert_eq!(
        outlook(WallKind::UsageWeekly, true, true, &c, true),
        Outlook::VendorResumes
    );
    assert_eq!(outlook(bucket, true, false, &c, true), Outlook::Resumes);
    assert_eq!(
        outlook(bucket, false, false, &c, true),
        Outlook::ResumesAndAsks(ExceptionKind::Bucket)
    );
    assert_eq!(
        outlook(WallKind::Spend, false, false, &c, true),
        Outlook::ResumesAndAsks(ExceptionKind::Spend)
    );
    assert_eq!(
        outlook(WallKind::Spend, true, false, &c, true),
        Outlook::Resumes
    );
    assert_eq!(
        outlook(WallKind::Auth, false, false, &c, true),
        Outlook::Person(ExceptionKind::Login)
    );
    assert_eq!(
        outlook(WallKind::Overloaded, false, false, &c, true),
        Outlook::Resumes
    );
    assert_eq!(
        outlook(WallKind::Context, false, false, &c, true),
        Outlook::Resumes
    );
    assert_eq!(
        outlook(WallKind::Memory, false, false, &c, true),
        Outlook::Resumes
    );
    assert_eq!(
        outlook(WallKind::UsageWeekly, true, false, &c, false),
        Outlook::Unattended
    );
    let off = SupervisorConfig {
        resume_limits: false,
        ..cfg()
    };
    assert_eq!(
        outlook(WallKind::UsageSession, true, false, &off, true),
        Outlook::Unattended
    );
    assert_eq!(
        outlook(WallKind::Spend, true, false, &off, true),
        Outlook::Person(ExceptionKind::Spend)
    );
}

/// Memory and login walls are nobody's whatever the switches say; a reset-less
/// bucket or spend limit resumes slowly and names the one action.
#[test]
fn a_wall_is_answered_by_the_harness_or_by_nobody() {
    let on = SupervisorConfig::default();
    let bucket = WallKind::ModelBucket { consent: false };
    for kind in [
        WallKind::UsageSession,
        WallKind::UsageWeekly,
        WallKind::Overloaded,
        WallKind::Context,
    ] {
        assert_eq!(answer(kind, true, &on, true), Answer::Resumes, "{kind:?}");
        assert_eq!(
            answer(kind, true, &on, false),
            Answer::Nobody,
            "{kind:?}: no supervisor here"
        );
    }
    assert_eq!(answer(bucket, true, &on, true), Answer::Resumes);
    assert_eq!(
        answer(bucket, false, &on, true),
        Answer::ResumesAsks("/model to switch")
    );
    assert_eq!(answer(WallKind::Spend, true, &on, true), Answer::Resumes);
    assert_eq!(
        answer(WallKind::Spend, false, &on, true),
        Answer::ResumesAsks("add credits")
    );
    // Nobody: a lost login, the memory banner, the owner's switches off.
    assert_eq!(answer(WallKind::Auth, false, &on, true), Answer::Nobody);
    assert_eq!(answer(WallKind::Memory, false, &on, true), Answer::Nobody);
    let off = SupervisorConfig {
        resume_limits: false,
        retry_api_errors: false,
        compact_on_context_wall: false,
        ..SupervisorConfig::default()
    };
    for kind in [
        WallKind::UsageWeekly,
        WallKind::Overloaded,
        WallKind::Context,
    ] {
        assert_eq!(answer(kind, true, &off, true), Answer::Nobody, "{kind:?}");
    }
    assert_eq!(answer(WallKind::Spend, true, &off, true), Answer::Nobody);
}
